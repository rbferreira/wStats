// wStatsTAP.dll: the stub wStats loads into explorer.exe through the XAML diagnostics
// API (InitializeXamlDiagnosticsEx, as TranslucentTB does).
//
// The stub never changes and never unloads. All the real work lives in wStatsCore.dll,
// which the stub loads from the path wStats publishes in shared memory and can swap on
// request (named event), so updating wStats never requires restarting Explorer.

#include <windows.h>
#include <ocidl.h>
#include <xamlOM.h>
#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.UI.Core.h>

#include <algorithm>
#include <atomic>
#include <string>
#include <utility>

#include "protocol.h"
#include "trace.h"

// {02282dab-1933-4e15-af04-8c5a60d07fe7}
static constexpr CLSID CLSID_WStatsTap = {0x02282dab, 0x1933, 0x4e15, {0xaf, 0x04, 0x8c, 0x5a, 0x60, 0xd0, 0x7f, 0xe7}};

using CoreAttach_t = HRESULT(__stdcall*)(IXamlDiagnostics*);
using CoreTreeChange_t = void(__stdcall*)(ParentChildRelation, VisualElement, VisualMutationType);
using CoreDetach_t = void(__stdcall*)();

// One watcher per Explorer. Injecting again (wStats cannot always tell whether the stub is
// already here) creates another watcher; it only asks the first one to reload, because
// two watchers would each keep a core and both would add elements to the taskbar.
static std::atomic<bool> g_serving{false};

struct Core
{
    HMODULE module = nullptr;
    CoreAttach_t attach = nullptr;
    CoreTreeChange_t treeChange = nullptr;
    CoreDetach_t detach = nullptr;
};

static std::wstring CorePathFromMapping()
{
    HANDLE map = OpenFileMappingW(FILE_MAP_READ, FALSE, wstats::kMappingName);
    if (!map) return {};
    std::wstring path;
    if (auto h = static_cast<const wstats::Header*>(MapViewOfFile(map, FILE_MAP_READ, 0, 0, wstats::kHeaderSize)))
    {
        if (h->magic == wstats::kMagic && h->version == wstats::kVersion) path.assign(h->corePath, wcsnlen(h->corePath, 260));
        UnmapViewOfFile(h);
    }
    CloseHandle(map);
    return path;
}

struct VisualTreeWatcher : winrt::implements<VisualTreeWatcher, IObjectWithSite, IVisualTreeServiceCallback2>
{
    winrt::com_ptr<IUnknown> m_site;
    winrt::com_ptr<IXamlDiagnostics> m_diag;
    Core m_core;
    SRWLOCK m_lock = SRWLOCK_INIT;

    bool LoadCore()
    {
        std::wstring path = CorePathFromMapping();
        if (path.empty())
        {
            wstats::Trace(L"stub: no core path in the mapping");
            return false;
        }
        Core c;
        c.module = LoadLibraryW(path.c_str());
        if (!c.module)
        {
            wstats::Trace(L"stub: LoadLibrary(%s) failed, error %lu", path.c_str(), GetLastError());
            return false;
        }
        c.attach = reinterpret_cast<CoreAttach_t>(GetProcAddress(c.module, "WStatsCoreAttach"));
        c.treeChange = reinterpret_cast<CoreTreeChange_t>(GetProcAddress(c.module, "WStatsCoreTreeChange"));
        c.detach = reinterpret_cast<CoreDetach_t>(GetProcAddress(c.module, "WStatsCoreDetach"));
        if (!c.attach || !c.treeChange || !c.detach || FAILED(c.attach(m_diag.get())))
        {
            wstats::Trace(L"stub: core %s rejected the attach", path.c_str());
            FreeLibrary(c.module);
            return false;
        }
        wstats::Trace(L"stub: core loaded: %s", path.c_str());
        AcquireSRWLockExclusive(&m_lock);
        m_core = c;
        ReleaseSRWLockExclusive(&m_lock);
        return true;
    }

    // Runs `f` on the XAML thread and waits for it. The wait has no timeout on purpose:
    // the lambda holds references into this frame, and unloading the old core while it
    // still runs would crash Explorer. A slow detach only delays the reload.
    template <typename F>
    void OnUiThread(F&& f)
    {
        winrt::com_ptr<::IInspectable> dispatcher;
        if (FAILED(m_diag->GetDispatcher(dispatcher.put())))
        {
            wstats::Trace(L"stub: no dispatcher; detaching on the stub thread");
            f();
            return;
        }
        HANDLE done = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        dispatcher.as<winrt::Windows::UI::Core::CoreDispatcher>().RunAsync(
            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal, [&] {
                f();
                SetEvent(done);
            });
        if (WaitForSingleObject(done, 10000) == WAIT_TIMEOUT)
        {
            wstats::Trace(L"stub: detach still running after 10 s, waiting");
            WaitForSingleObject(done, INFINITE);
        }
        CloseHandle(done);
    }

    // Background thread: subscribe to the tree, then serve reload requests forever.
    // Advising also replays the whole tree, so the core sees every taskbar. Explorer can
    // refuse it (E_UNEXPECTED, seen after the Widgets button was turned off); a refusal is
    // retried with a growing delay, reloads still being served meanwhile.
    void Serve()
    {
        auto tree = m_diag.as<IVisualTreeService3>();
        HANDLE reload = CreateEventW(nullptr, FALSE, FALSE, wstats::kReloadEvent);
        LoadCore();
        HRESULT hr = tree->AdviseVisualTreeChange(this);
        wstats::Trace(L"stub: serving (advise 0x%08X)", static_cast<unsigned>(hr));
        bool advised = SUCCEEDED(hr);
        DWORD retry = 1000;

        while (reload)
        {
            DWORD wait = WaitForSingleObject(reload, advised ? INFINITE : retry);
            if (wait == WAIT_TIMEOUT)
            {
                hr = tree->AdviseVisualTreeChange(this);
                advised = SUCCEEDED(hr);
                wstats::Trace(L"stub: advise retried after %lu ms: 0x%08X", retry, static_cast<unsigned>(hr));
                retry = std::min<DWORD>(retry * 2, 60000);
                continue;
            }
            if (wait != WAIT_OBJECT_0) break;
            wstats::Trace(L"stub: reload requested");
            if (advised)
            {
                hr = tree->UnadviseVisualTreeChange(this);
                if (FAILED(hr)) wstats::Trace(L"stub: unadvise failed 0x%08X", static_cast<unsigned>(hr));
            }
            OnUiThread([this] {
                AcquireSRWLockExclusive(&m_lock);
                Core old = std::exchange(m_core, Core{});
                ReleaseSRWLockExclusive(&m_lock);
                if (old.module)
                {
                    old.detach();
                    FreeLibrary(old.module);
                }
            });
            LoadCore();
            hr = tree->AdviseVisualTreeChange(this);
            advised = SUCCEEDED(hr);
            retry = 1000;
            if (!advised) wstats::Trace(L"stub: advise after reload failed 0x%08X", static_cast<unsigned>(hr));
        }
    }

    HRESULT STDMETHODCALLTYPE SetSite(IUnknown* site) noexcept override
    {
        m_site.copy_from(site);
        if (!site) return S_OK;
        if (g_serving.exchange(true))
        {
            wstats::Trace(L"stub: injected again; handing over to the running watcher");
            if (HANDLE reload = OpenEventW(EVENT_MODIFY_STATE, FALSE, wstats::kReloadEvent))
            {
                SetEvent(reload);
                CloseHandle(reload);
            }
            return S_OK;
        }
        m_diag = m_site.as<IXamlDiagnostics>();
        // Advising from the thread that called SetSite deadlocks; use our own thread.
        AddRef();
        HANDLE thread = CreateThread(nullptr, 0, [](void* p) -> DWORD {
            auto self = static_cast<VisualTreeWatcher*>(p);
            self->Serve();
            self->Release();
            return 0;
        }, this, 0, nullptr);
        if (thread) CloseHandle(thread);
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE GetSite(REFIID riid, void** out) noexcept override
    {
        return m_site ? m_site->QueryInterface(riid, out) : E_FAIL;
    }

    HRESULT STDMETHODCALLTYPE OnVisualTreeChange(ParentChildRelation rel, VisualElement el, VisualMutationType type) noexcept override
    {
        AcquireSRWLockShared(&m_lock);
        auto change = m_core.treeChange;
        ReleaseSRWLockShared(&m_lock);
        if (change) change(rel, el, type);
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE OnElementStateChanged(InstanceHandle, VisualElementState, LPCWSTR) noexcept override
    {
        return S_OK;
    }
};

struct Factory : winrt::implements<Factory, IClassFactory>
{
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer, REFIID riid, void** out) noexcept override
    {
        if (outer) return CLASS_E_NOAGGREGATION;
        try
        {
            return winrt::make<VisualTreeWatcher>().as<IUnknown>()->QueryInterface(riid, out);
        }
        catch (...)
        {
            return winrt::to_hresult();
        }
    }

    HRESULT STDMETHODCALLTYPE LockServer(BOOL) noexcept override { return S_OK; }
};

extern "C" HRESULT __stdcall DllGetClassObject(REFCLSID clsid, REFIID riid, void** out)
{
    if (clsid != CLSID_WStatsTap) return CLASS_E_CLASSNOTAVAILABLE;
    try
    {
        return winrt::make<Factory>().as<IUnknown>()->QueryInterface(riid, out);
    }
    catch (...)
    {
        return winrt::to_hresult();
    }
}

extern "C" HRESULT __stdcall DllCanUnloadNow()
{
    return S_FALSE;
}
