// wStatsCore.dll: runs inside explorer.exe, on the taskbars' XAML threads.
//
// It adds one element to a taskbar frame and makes the taskbar's own layout reserve room
// for it, so the icons move aside exactly as they do for the Widgets button:
//   - Left:  the Widgets button gets a right margin as wide as the strip, and the
//            element sits in that margin (between Widgets and Start).
//   - Right: the item repeater gets a right margin and the element sits in it, clear of
//            the notification area.
//
// The element is just an Image. wStats renders the pixels (Direct2D, in Rust) into
// shared memory; this code copies them into a WriteableBitmap and forwards pointer
// input back. When wStats goes away, everything here is undone.
//
// Threading: each taskbar (one per monitor) may run on its own UI thread, and XAML
// objects can only be touched from their own thread. So every frame ("slot") gets its
// own DispatcherTimer on its own thread and never touches another slot's objects. The
// slot list and the island list are shared and guarded by a lock.

#include <windows.h>
#include <xamlOM.h>
#include <windows.ui.xaml.hosting.desktopwindowxamlsource.h>
#undef GetCurrentTime // winbase.h macro collides with a XAML method name
#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Storage.Streams.h>
#include <winrt/Windows.UI.h>
#include <winrt/Windows.UI.Core.h>
#include <winrt/Windows.UI.Input.h>
#include <winrt/Windows.UI.Xaml.h>
#include <winrt/Windows.UI.Xaml.Automation.h>
#include <winrt/Windows.UI.Xaml.Controls.h>
#include <winrt/Windows.UI.Xaml.Hosting.h>
#include <winrt/Windows.UI.Xaml.Input.h>
#include <winrt/Windows.UI.Xaml.Media.h>
#include <winrt/Windows.UI.Xaml.Media.Imaging.h>

#include <atomic>
#include <cmath>
#include <cstdio>
#include <memory>
#include <mutex>
#include <vector>

#include "protocol.h"
#include "trace.h"

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Input;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Windows::UI::Xaml::Media::Imaging;

namespace
{
constexpr double kGap = 4.0; // DIPs between the strip and its neighbours

// ---- shared memory ----------------------------------------------------------------------

struct Shared
{
    HANDLE map = nullptr;
    wstats::Header* header = nullptr;
    const uint8_t* pixels = nullptr;

    bool Open()
    {
        if (header) return true;
        map = OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, FALSE, wstats::kMappingName);
        if (!map) return false;
        auto base = static_cast<uint8_t*>(MapViewOfFile(map, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, wstats::kMappingSize));
        if (!base)
        {
            CloseHandle(map);
            map = nullptr;
            return false;
        }
        header = reinterpret_cast<wstats::Header*>(base);
        pixels = base + wstats::kHeaderSize;
        return true;
    }

    void Close()
    {
        if (header) UnmapViewOfFile(header);
        if (map) CloseHandle(map);
        *this = {};
    }

    bool Valid() const
    {
        return header && header->magic == wstats::kMagic && header->version == wstats::kVersion;
    }

    HWND Host() const { return reinterpret_cast<HWND>(static_cast<uintptr_t>(header->hostHwnd)); }
};

// ---- state ------------------------------------------------------------------------------

/// One taskbar frame and everything we added to it. Touched only on its own thread.
struct Slot
{
    InstanceHandle handle = 0;
    DWORD thread = 0;
    Windows::UI::Core::CoreDispatcher dispatcher{nullptr};
    HWND taskbar = nullptr; // Shell_TrayWnd / Shell_SecondaryTrayWnd, once known
    weak_ref<FrameworkElement> frame;
    Panel root{nullptr};
    Panel repeater{nullptr};
    Grid host{nullptr};
    Image image{nullptr};
    WriteableBitmap bitmap{nullptr};
    FrameworkElement widgets{nullptr};
    Thickness widgetsMargin{}, repeaterMargin{};
    TranslateTransform shift{nullptr}; // recentres the icons; see Recenter
    Transform repeaterTransform{nullptr};
    DispatcherTimer timer{nullptr};
    DispatcherTimer::Tick_revoker tick;
    FrameworkElement::PointerMoved_revoker moved;
    FrameworkElement::PointerExited_revoker exited;
    FrameworkElement::PointerPressed_revoker pressed;
    FrameworkElement::PointerReleased_revoker released;
    FrameworkElement::RightTapped_revoker rightTapped;
    long shownSeq = -1;
    uint32_t width = 0, height = 0;
    int traced = -1; // last state written to the trace, so it logs changes only
    std::vector<uint8_t> copy;
};

/// A taskbar's XAML island, used to tie a frame to its window (and so its monitor).
struct Source
{
    InstanceHandle handle = 0;
    DWORD thread = 0;
    weak_ref<Hosting::DesktopWindowXamlSource> source;
    HWND hwnd = nullptr;
};

std::mutex g_lock; // guards g_slots, g_sources and opening g_shared
com_ptr<IXamlDiagnostics> g_diag;
std::vector<Source> g_sources;
// Heap-allocated so pointer-capturing handlers stay valid while the list changes.
std::vector<std::unique_ptr<Slot>> g_slots;
Shared g_shared;
std::atomic<uint32_t> g_attached{0};

/// Last error from a timer, in %TEMP%\wstats-core.log (overwritten, so it never grows).
void LogError(HRESULT hr, const wchar_t* message)
{
    static std::atomic<HRESULT> last{S_OK};
    if (last.exchange(hr) == hr) return;
    wstats::Trace(L"core: timer error 0x%08X %s", static_cast<unsigned>(hr), message);
    wchar_t path[MAX_PATH];
    GetTempPathW(MAX_PATH, path);
    wcscat_s(path, L"wstats-core.log");
    FILE* f = nullptr;
    if (_wfopen_s(&f, path, L"w, ccs=UTF-8") != 0 || !f) return;
    fwprintf(f, L"0x%08X %s\n", static_cast<unsigned>(hr), message);
    fclose(f);
}

bool Near(double a, double b) { return std::abs(a - b) < 0.5; }

void SetMargin(FrameworkElement const& el, Thickness t)
{
    Thickness cur = el.Margin();
    if (!Near(cur.Left, t.Left) || !Near(cur.Right, t.Right) || !Near(cur.Top, t.Top) || !Near(cur.Bottom, t.Bottom)) el.Margin(t);
}

template <typename T>
T FindChild(DependencyObject const& parent, std::wstring_view name)
{
    for (int i = 0, n = VisualTreeHelper::GetChildrenCount(parent); i < n; i++)
    {
        auto child = VisualTreeHelper::GetChild(parent, i).try_as<T>();
        if (child && child.Name() == name) return child;
    }
    return nullptr;
}

FrameworkElement FindWidgets(Panel const& repeater)
{
    for (int i = 0, n = VisualTreeHelper::GetChildrenCount(repeater); i < n; i++)
    {
        auto child = VisualTreeHelper::GetChild(repeater, i).try_as<FrameworkElement>();
        if (child && Automation::AutomationProperties::GetAutomationId(child) == L"WidgetsButton") return child;
    }
    return nullptr;
}

double Scale(FrameworkElement const& el)
{
    auto root = el.XamlRoot();
    return root ? root.RasterizationScale() : 1.0;
}

/// Scale wStats rendered at. Sizing the element by it (not by the frame's own scale)
/// keeps the strip the same size in DIPs on every monitor.
double RenderScale()
{
    uint32_t s = g_shared.header->renderScale1000;
    return s ? s / 1000.0 : 1.0;
}

DependencyObject Top(DependencyObject d)
{
    while (auto parent = VisualTreeHelper::GetParent(d)) d = parent;
    return d;
}

// ---- which monitor ----------------------------------------------------------------------

BOOL CALLBACK CollectTaskbars(HWND hwnd, LPARAM out)
{
    wchar_t cls[64];
    if (GetClassNameW(hwnd, cls, 64) && (wcscmp(cls, L"Shell_TrayWnd") == 0 || wcscmp(cls, L"Shell_SecondaryTrayWnd") == 0))
        reinterpret_cast<std::vector<HWND>*>(out)->push_back(hwnd);
    return TRUE;
}

/// The taskbar window the frame is drawn in. Taskbars are owned by the thread that runs
/// their XAML, so usually there is exactly one on this thread; when a thread hosts
/// several, the frame's island tells them apart.
HWND TaskbarOf(Slot& f)
{
    if (f.taskbar) return f.taskbar;
    std::vector<HWND> mine;
    EnumThreadWindows(GetCurrentThreadId(), CollectTaskbars, reinterpret_cast<LPARAM>(&mine));
    if (mine.size() == 1) return f.taskbar = mine[0];

    auto frame = f.frame.get();
    if (!frame) return nullptr;
    auto top = Top(frame);
    std::vector<Source> sources;
    {
        std::lock_guard lock(g_lock);
        sources = g_sources;
    }
    for (auto& s : sources)
    {
        if (s.thread != GetCurrentThreadId()) continue;
        auto source = s.source.get();
        auto content = source ? source.Content() : nullptr;
        if (content && Top(content) == top) return f.taskbar = GetAncestor(s.hwnd, GA_ROOT);
    }
    return nullptr;
}

/// Whether this frame should host the element: every taskbar, or the chosen monitor's
/// (the primary by default).
bool OnTarget(Slot& f)
{
    auto h = g_shared.header;
    if (h->allMonitors) return true;
    HWND taskbar = TaskbarOf(f);
    if (!taskbar) return false;
    RECT want{h->monLeft, h->monTop, h->monRight, h->monBottom};
    HMONITOR target = want.right > want.left ? MonitorFromRect(&want, MONITOR_DEFAULTTOPRIMARY) : MonitorFromPoint({0, 0}, MONITOR_DEFAULTTOPRIMARY);
    return MonitorFromWindow(taskbar, MONITOR_DEFAULTTONEAREST) == target;
}

// ---- the element ------------------------------------------------------------------------

void Post(Slot& f, wstats::PointerEvent kind, Point pos)
{
    if (!g_shared.Valid()) return;
    // Positions go out in render pixels, which is what wStats' layout is measured in.
    double rs = RenderScale();
    int x = static_cast<int>(pos.X * rs), y = static_cast<int>(pos.Y * rs);
    if (kind == wstats::LeftClick || kind == wstats::RightClick)
    {
        // The element's screen origin = cursor (screen) - pointer (element, in this
        // monitor's pixels). wStats puts the popup right above the clicked item.
        double s = Scale(f.host);
        POINT cursor;
        GetCursorPos(&cursor);
        g_shared.header->elemX = cursor.x - static_cast<int>(pos.X * s);
        g_shared.header->elemY = cursor.y - static_cast<int>(pos.Y * s);
        g_shared.header->clickScale1000 = static_cast<uint32_t>(s * 1000);
        // The click happened in Explorer; let wStats take the foreground for its popup.
        DWORD pid = 0;
        GetWindowThreadProcessId(g_shared.Host(), &pid);
        AllowSetForegroundWindow(pid);
    }
    PostMessageW(g_shared.Host(), wstats::kMsgPointer, kind, MAKELPARAM(x, y));
}

/// Removes the element and restores the taskbar's margins. Must run on the slot's thread.
void Teardown(Slot& f)
{
    f.moved.revoke();
    f.exited.revoke();
    f.pressed.revoke();
    f.released.revoke();
    f.rightTapped.revoke();
    if (f.widgets) f.widgets.Margin(f.widgetsMargin);
    if (f.repeater) f.repeater.Margin(f.repeaterMargin);
    if (f.repeater && f.shift) f.repeater.RenderTransform(f.repeaterTransform);
    f.shift = nullptr;
    f.repeaterTransform = nullptr;
    if (f.root && f.host)
    {
        uint32_t index;
        if (f.root.Children().IndexOf(f.host, index)) f.root.Children().RemoveAt(index);
        g_attached--;
    }
    f.host = nullptr;
    f.image = nullptr;
    f.bitmap = nullptr;
    f.widgets = nullptr;
    f.shownSeq = -1;
}

/// Teardown plus stopping the slot's timer: nothing of ours runs on it afterwards.
void Shutdown(Slot& f)
{
    f.tick.revoke();
    if (f.timer) f.timer.Stop();
    f.timer = nullptr;
    Teardown(f);
}

/// Whether the element fell out of the frame's live tree. Explorer can rebuild a frame's
/// content without removing the frame, and the element would then sit in a detached tree
/// that is never drawn. A few calls per tick.
bool Detached(Slot& f)
{
    auto frame = f.frame.get();
    if (!frame) return true;
    return VisualTreeHelper::GetParent(f.host) != f.root || FindChild<Panel>(frame, L"RootGrid") != f.root ||
           FindChild<Panel>(f.root, L"TaskbarFrameRepeater") != f.repeater;
}

/// Finds the frame's parts and inserts the element. False until the frame's template has
/// been applied (its children are created after the frame itself appears).
bool Setup(Slot& f)
{
    if (f.host) return true;
    auto frame = f.frame.get();
    if (!frame) return false;
    f.root = FindChild<Panel>(frame, L"RootGrid");
    if (!f.root) return false;
    f.repeater = FindChild<Panel>(f.root, L"TaskbarFrameRepeater");
    if (!f.repeater) return false;
    f.repeaterMargin = f.repeater.Margin();

    f.image = Image();
    f.image.Stretch(Stretch::Fill);
    f.image.IsHitTestVisible(false);
    f.host = Grid();
    // A transparent (not null) background is what makes the empty area hit-testable.
    f.host.Background(SolidColorBrush(winrt::Windows::UI::Colors::Transparent()));
    f.host.HorizontalAlignment(HorizontalAlignment::Left);
    f.host.VerticalAlignment(VerticalAlignment::Center);
    f.host.Children().Append(f.image);
    f.root.Children().Append(f.host);
    g_attached++;

    Slot* self = &f;
    f.moved = f.host.PointerMoved(auto_revoke, [self](auto&&, PointerRoutedEventArgs const& e) {
        Post(*self, wstats::Move, e.GetCurrentPoint(self->host).Position());
    });
    f.exited = f.host.PointerExited(auto_revoke, [self](auto&&, PointerRoutedEventArgs const&) {
        Post(*self, wstats::Leave, {});
    });
    f.pressed = f.host.PointerPressed(auto_revoke, [](auto&&, PointerRoutedEventArgs const& e) { e.Handled(true); });
    f.released = f.host.PointerReleased(auto_revoke, [self](auto&&, PointerRoutedEventArgs const& e) {
        auto point = e.GetCurrentPoint(self->host);
        bool right = point.Properties().PointerUpdateKind() == winrt::Windows::UI::Input::PointerUpdateKind::RightButtonReleased;
        Post(*self, right ? wstats::RightClick : wstats::LeftClick, point.Position());
        e.Handled(true);
    });
    // Keep the taskbar's own context menu from opening over ours.
    f.rightTapped = f.host.RightTapped(auto_revoke, [](auto&&, RightTappedRoutedEventArgs const& e) { e.Handled(true); });
    return true;
}

/// How far the notification area reaches into the frame from its right edge. The tray
/// is a sibling drawn over the right end of the frame, so right-aligned content must stay
/// clear of it.
double TrayInset(Slot& f)
{
    auto frame = f.frame.get();
    if (!frame) return 0;
    auto parent = VisualTreeHelper::GetParent(frame);
    if (!parent) return 0;
    for (int i = 0, n = VisualTreeHelper::GetChildrenCount(parent); i < n; i++)
    {
        auto child = VisualTreeHelper::GetChild(parent, i).try_as<FrameworkElement>();
        if (!child || get_class_name(child) != L"SystemTray.SystemTrayFrame") continue;
        auto x = child.TransformToVisual(f.root).TransformPoint({0, 0}).X;
        return std::max(0.0, f.root.ActualWidth() - x);
    }
    return 0;
}

/// Whether the Widgets button is on the bar. Turned off, it is not removed: the repeater
/// parks it out of view (x around -10000) with its size intact, and placing the strip
/// after it put the strip out of view too.
bool WidgetsShown(Slot& f)
{
    if (!f.widgets || f.widgets.ActualWidth() <= 0 || f.widgets.Visibility() != Visibility::Visible) return false;
    double x = f.widgets.TransformToVisual(f.repeater).TransformPoint({0, 0}).X;
    return x + f.widgets.ActualWidth() > 0 && x < f.repeater.ActualWidth();
}

/// Whether the taskbar centres its icons (the Windows 11 default) rather than aligning
/// them left. Every frame asks on every tick, so the setting is read at most once a second.
bool Centered()
{
    static std::atomic<ULONGLONG> readAt{0};
    static std::atomic<bool> centered{true};
    ULONGLONG now = GetTickCount64();
    if (now - readAt > 1000)
    {
        DWORD value = 1, size = sizeof value;
        if (RegGetValueW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced", L"TaskbarAl", RRF_RT_REG_DWORD,
                         nullptr, &value, &size) != ERROR_SUCCESS)
            value = 1; // never set: centred
        centered = value != 0;
        readAt = now;
    }
    return centered;
}

/// Puts centred icons back in the middle of the bar. The repeater centres its icons in
/// whatever width it is given, so reserving the strip's room on one side (`reserved`,
/// DIPs: positive on the left, negative on the right) moves them half of it away. A
/// render transform moves them back, only as far as the free space beside them allows.
/// Not a matching margin on the other side: that leaves less room, and on a narrow bar
/// Windows sends icons to the overflow menu.
void Recenter(Slot& f, double reserved)
{
    double shift = 0;
    if (reserved != 0 && Centered())
    {
        // The icons' extent, from layout positions (the repeater's own transform is not
        // part of them, so this does not feed back on itself).
        double width = f.repeater.ActualWidth(), first = width, last = 0;
        for (int i = 0, n = VisualTreeHelper::GetChildrenCount(f.repeater); i < n; i++)
        {
            auto child = VisualTreeHelper::GetChild(f.repeater, i).try_as<FrameworkElement>();
            if (!child || child.ActualWidth() <= 0 || child.Visibility() != Visibility::Visible) continue;
            double x = child.TransformToVisual(f.repeater).TransformPoint({0, 0}).X;
            if (x + child.ActualWidth() <= 0 || x >= width) continue; // recycled, parked out of view
            first = std::min(first, x);
            last = std::max(last, x + child.ActualWidth());
        }
        if (last > first) shift = reserved > 0 ? -std::min(reserved / 2, first) : std::min(-reserved / 2, width - last);
    }
    if (!f.shift)
    {
        if (Near(shift, 0)) return;
        f.shift = TranslateTransform();
    }
    // The repeater comes with an identity MatrixTransform; it is put back on teardown.
    // Should the taskbar set another one later, ours replaces it again.
    auto current = f.repeater.RenderTransform();
    if (current != f.shift)
    {
        f.repeaterTransform = current;
        f.repeater.RenderTransform(f.shift);
    }
    if (!Near(f.shift.X(), shift)) f.shift.X(shift);
}

void Layout(Slot& f)
{
    double s = RenderScale();
    double w = f.width / s, h = f.height / s;
    f.host.Width(w);
    f.host.Height(h);
    f.image.Width(w);
    f.image.Height(h);
    double reserve = w > 0 ? w + 2 * kGap : 0;

    auto widgets = FindWidgets(f.repeater);
    if (widgets != f.widgets)
    {
        if (f.widgets) f.widgets.Margin(f.widgetsMargin);
        f.widgets = widgets;
        if (widgets) f.widgetsMargin = widgets.Margin();
    }

    bool left = g_shared.header->position == wstats::Left;
    bool widgetsShown = WidgetsShown(f);
    double reserved = 0; // room taken from the repeater, for Recenter
    if (left && widgetsShown)
    {
        // The Widgets button is wider than what it draws (it keeps ~58 DIP of slack on
        // its right). Sit right after the visible part, use the slack first and reserve
        // only what is still missing.
        auto origin = f.widgets.TransformToVisual(f.root).TransformPoint({0, 0});
        double slotRight = origin.X + f.widgets.ActualWidth();
        double visibleRight = slotRight;
        if (auto panel = FindChild<FrameworkElement>(f.widgets, L"ExperienceToggleButtonRootPanel"))
            visibleRight = panel.TransformToVisual(f.root).TransformPoint({0, 0}).X + panel.ActualWidth();
        double x = visibleRight + kGap;
        double extra = w > 0 ? std::max(0.0, x + w + kGap - slotRight) : 0.0;
        SetMargin(f.widgets, {f.widgetsMargin.Left, f.widgetsMargin.Top, f.widgetsMargin.Right + extra, f.widgetsMargin.Bottom});
        SetMargin(f.repeater, f.repeaterMargin);
        f.host.HorizontalAlignment(HorizontalAlignment::Left);
        SetMargin(f.host, {x, 0, 0, 0});
    }
    else if (left)
    {
        if (f.widgets) SetMargin(f.widgets, f.widgetsMargin);
        SetMargin(f.repeater, {f.repeaterMargin.Left + reserve, f.repeaterMargin.Top, f.repeaterMargin.Right, f.repeaterMargin.Bottom});
        f.host.HorizontalAlignment(HorizontalAlignment::Left);
        SetMargin(f.host, {f.repeaterMargin.Left + kGap, 0, 0, 0});
        reserved = reserve;
    }
    else
    {
        if (f.widgets) SetMargin(f.widgets, f.widgetsMargin);
        double tray = TrayInset(f);
        SetMargin(f.repeater, {f.repeaterMargin.Left, f.repeaterMargin.Top, f.repeaterMargin.Right + tray + reserve, f.repeaterMargin.Bottom});
        f.host.HorizontalAlignment(HorizontalAlignment::Right);
        SetMargin(f.host, {0, 0, tray + kGap, 0});
        reserved = -(tray + reserve);
    }
    Recenter(f, reserved);
}

/// Copies a stable frame out of shared memory (seqlock). False if nothing new or wStats
/// was mid-write.
bool ReadPixels(Slot& f)
{
    auto h = g_shared.header;
    long seq = h->seq;
    if (seq & 1 || seq == f.shownSeq) return false;
    uint32_t w = h->width, hh = h->height;
    if (w == 0 || hh == 0 || static_cast<uint64_t>(w) * hh > wstats::kMaxPixels) return false;
    f.copy.resize(static_cast<size_t>(w) * hh * 4);
    memcpy(f.copy.data(), g_shared.pixels, f.copy.size());
    MemoryBarrier();
    if (h->seq != seq) return false;
    f.shownSeq = seq;
    f.width = w;
    f.height = hh;
    return true;
}

void Present(Slot& f)
{
    if (!f.bitmap || f.bitmap.PixelWidth() != static_cast<int>(f.width) || f.bitmap.PixelHeight() != static_cast<int>(f.height))
    {
        f.bitmap = WriteableBitmap(f.width, f.height);
        f.image.Source(f.bitmap);
    }
    auto buffer = f.bitmap.PixelBuffer();
    memcpy(buffer.data(), f.copy.data(), std::min<size_t>(buffer.Capacity(), f.copy.size()));
    f.bitmap.Invalidate();
}

/// Writes the slot's state to the trace when it changes.
void Note(Slot& f, int state)
{
    if (f.traced == state) return;
    f.traced = state;
    static const wchar_t* names[] = {L"no shared memory", L"wStats gone", L"other monitor", L"waiting for the frame", L"showing"};
    wstats::Trace(L"core: frame %llu (taskbar %p): %s, attached %u", static_cast<unsigned long long>(f.handle), f.taskbar, names[state],
                  static_cast<unsigned>(g_attached));
}

void Tick(Slot& f)
{
    {
        std::lock_guard lock(g_lock);
        if (!g_shared.Open() || !g_shared.Valid())
        {
            Note(f, 0);
            return;
        }
    }
    if (!IsWindow(g_shared.Host()))
    {
        // wStats is gone: put this taskbar back exactly as it was.
        if (f.host) Teardown(f);
        g_shared.header->attached = g_attached;
        Note(f, 1);
        return;
    }
    InterlockedIncrement(&g_shared.header->heartbeat);
    g_shared.header->tapPid = GetCurrentProcessId();

    if (f.host && OnTarget(f) && Detached(f))
    {
        wstats::Trace(L"core: frame %llu was rebuilt by Explorer; inserting again", static_cast<unsigned long long>(f.handle));
        Teardown(f);
    }

    if (!OnTarget(f))
    {
        if (f.host) Teardown(f);
        Note(f, 2);
    }
    else if (Setup(f))
    {
        if (ReadPixels(f) || !f.bitmap) Present(f);
        if (f.width) Layout(f);
        Note(f, 4);
    }
    else
    {
        Note(f, 3);
    }
    g_shared.header->attached = g_attached;
}

/// Starts the slot's timer on the calling thread (the slot's own UI thread).
void StartTimer(Slot& f)
{
    f.timer = DispatcherTimer();
    f.timer.Interval(std::chrono::milliseconds(100));
    Slot* self = &f;
    f.tick = f.timer.Tick(auto_revoke, [self](auto&&, auto&&) {
        try
        {
            Tick(*self);
        }
        catch (hresult_error const& e)
        {
            // Never let an exception reach Explorer's message loop; leave a trace instead.
            LogError(e.code(), e.message().c_str());
        }
        catch (...)
        {
            LogError(E_FAIL, L"unknown exception");
        }
    });
    f.timer.Start();
}
} // namespace

extern "C" HRESULT __stdcall WStatsCoreAttach(IXamlDiagnostics* diag)
{
    g_diag.copy_from(diag);
    std::lock_guard lock(g_lock);
    if (g_shared.Open() && g_shared.Valid())
    {
        // Whatever the previous core reported is no longer true.
        g_shared.header->attached = 0;
        long loads = InterlockedIncrement(&g_shared.header->coreLoads);
        wstats::Trace(L"core: attached, load #%ld", loads);
    }
    else
    {
        wstats::Trace(L"core: attached without shared memory");
    }
    return S_OK;
}

// Called on the thread that owns the element.
extern "C" void __stdcall WStatsCoreTreeChange(ParentChildRelation, VisualElement el, VisualMutationType type)
{
    try
    {
        if (!el.Type) return;
        if (wcscmp(el.Type, L"Windows.UI.Xaml.Hosting.DesktopWindowXamlSource") == 0)
        {
            std::lock_guard lock(g_lock);
            if (type == Add)
            {
                // A retried advise replays the tree again.
                for (auto& s : g_sources)
                    if (s.handle == el.Handle) return;
                com_ptr<::IInspectable> obj;
                if (FAILED(g_diag->GetIInspectableFromHandle(el.Handle, reinterpret_cast<::IInspectable**>(obj.put())))) return;
                Source s;
                s.handle = el.Handle;
                s.thread = GetCurrentThreadId();
                s.source = obj.as<Hosting::DesktopWindowXamlSource>();
                obj.as<IDesktopWindowXamlSourceNative>()->get_WindowHandle(&s.hwnd);
                g_sources.push_back(s);
            }
            else
            {
                std::erase_if(g_sources, [&](Source const& s) { return s.handle == el.Handle; });
            }
            return;
        }
        if (wcscmp(el.Type, L"Taskbar.TaskbarFrame") != 0) return;

        if (type == Add)
        {
            com_ptr<::IInspectable> obj;
            if (FAILED(g_diag->GetIInspectableFromHandle(el.Handle, reinterpret_cast<::IInspectable**>(obj.put())))) return;
            auto frame = obj.as<FrameworkElement>();
            std::lock_guard lock(g_lock);
            for (auto& s : g_slots)
                if (s->handle == el.Handle) return;
            auto slot = std::make_unique<Slot>();
            slot->handle = el.Handle;
            slot->thread = GetCurrentThreadId();
            slot->dispatcher = frame.Dispatcher();
            slot->frame = frame;
            StartTimer(*slot);
            wstats::Trace(L"core: taskbar frame %llu added (%zu frames)", static_cast<unsigned long long>(el.Handle), g_slots.size() + 1);
            g_slots.push_back(std::move(slot));
        }
        else
        {
            std::unique_ptr<Slot> gone;
            {
                std::lock_guard lock(g_lock);
                for (auto it = g_slots.begin(); it != g_slots.end(); ++it)
                {
                    if ((*it)->handle != el.Handle) continue;
                    gone = std::move(*it);
                    g_slots.erase(it);
                    break;
                }
            }
            if (gone)
            {
                wstats::Trace(L"core: taskbar frame %llu removed", static_cast<unsigned long long>(el.Handle));
                Shutdown(*gone);
            }
        }
    }
    catch (hresult_error const& e)
    {
        wstats::Trace(L"core: tree change failed 0x%08X %s", static_cast<unsigned>(e.code()), e.message().c_str());
    }
    catch (...)
    {
        wstats::Trace(L"core: tree change failed");
    }
}

// Called on the diagnostics thread before the DLL is unloaded: every slot is shut down on
// its own thread, and this waits for each.
extern "C" void __stdcall WStatsCoreDetach()
{
    std::vector<std::unique_ptr<Slot>> slots;
    {
        std::lock_guard lock(g_lock);
        slots.swap(g_slots);
        g_sources.clear();
    }
    wstats::Trace(L"core: detaching from %zu frames", slots.size());
    for (auto& s : slots)
    {
        try
        {
            if (s->thread == GetCurrentThreadId() || !s->dispatcher)
            {
                Shutdown(*s);
                continue;
            }
            HANDLE done = CreateEventW(nullptr, TRUE, FALSE, nullptr);
            Slot* raw = s.get();
            s->dispatcher.RunAsync(Windows::UI::Core::CoreDispatcherPriority::High, [raw, done] {
                try
                {
                    Shutdown(*raw);
                }
                catch (...)
                {
                }
                SetEvent(done);
            });
            if (WaitForSingleObject(done, 5000) == WAIT_TIMEOUT)
            {
                // The shutdown is still queued on that thread and will run later, with
                // this slot and this DLL's code. Keep both alive: leak the slot and the
                // event, and pin the DLL so the stub's FreeLibrary cannot unload it.
                wstats::Trace(L"core: frame %llu did not shut down in 5 s; pinning this core", static_cast<unsigned long long>(raw->handle));
                HMODULE self;
                GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN, reinterpret_cast<LPCWSTR>(&WStatsCoreDetach), &self);
                s.release();
                continue;
            }
            CloseHandle(done);
        }
        catch (...)
        {
        }
    }
    try
    {
        if (g_shared.header) g_shared.header->attached = 0;
        g_shared.Close();
        g_diag = nullptr;
    }
    catch (...)
    {
    }
}
