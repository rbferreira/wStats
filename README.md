<div align="center">

<img src="src-tauri/icons/128x128@2x.png" alt="" width="96">

# wStats

**English** · [Português](README.pt-BR.md)

**A system monitor that lives inside the Windows taskbar.**
Good-looking, light, and always in sight.

<img src="docs/img/taskbar.png" alt="wStats' ten items in the taskbar: CPU, GPU, NPU, memory, network, disk, battery, temperature, fan and power" width="840">

**Truly light:** under 0.2% CPU and, in memory-saving mode, about 18 MB of RAM.

</div>

---

## What it is

wStats puts small graphs of **CPU, GPU, NPU, memory, disk, network, battery, temperature, fans and power draw** right in the taskbar. Clicking an item opens a panel with the details: up to 24 hours of history, processes, cores, sensors and shortcuts to the Windows tools.

What sets it apart from other monitors is that **wStats is part of the taskbar**: it does not float on top of it. App icons make room for it the way they do for the Widgets button, and everything reflows when you turn an item on or off.

And it fits your taste: **each item's name goes above, below, beside (on the left or the right) or nowhere**, and with no name line the number grows to fill the bar. The graphics can be turned off too, leaving the text alone.

<div align="center">
<img src="docs/img/labels.png" alt="The four ways to show the labels: above, below, beside and none" width="100%">
</div>

For a lighter taskbar, the **compact mode** puts two items per column, numbers only, each tagged by an icon or its name, on the left or the right. All ten items fit in less than half the space, and each number still opens its panel.

<div align="center">
<img src="docs/img/compact.png" alt="Compact mode in its four combinations: icon or label, on the left or the right" width="60%">
</div>

<div align="center">
<img src="docs/img/popups-dark.png" alt="CPU, memory, NPU, temperature and fan panels in the dark theme" width="100%">
<br><sub>Dark theme: CPU, memory, NPU, temperature and fans</sub>
<br><br>
<img src="docs/img/popups-light.png" alt="Network, disk, power and battery panels in the light theme" width="100%">
<br><sub>Light theme: network, disks, power and battery</sub>
</div>

## Features

| Item | In the taskbar | In the panel |
|---|---|---|
| **CPU** | usage with a user/system graph | performance and efficiency cores, frequency, processes, GPU, uptime |
| **GPU** | usage (Task Manager's rule) | 3D/video/copy engines, dedicated and shared memory, temperature, fan and clock when the driver reports them, board power (NVIDIA) |
| **NPU** | a chip that fills with the load; shown only on machines with an NPU (Intel AI Boost, AMD XDNA, Qualcomm Hexagon) | 24 h history, memory, the apps using the NPU |
| **Memory** | usage ring | pressure, breakdown (apps, kernel, cache), processes, page file |
| **Disks** | space used on C: | volumes, read/write with history, the processes reading and writing most |
| **Network** | upload and download, in one unit | history, Wi-Fi (SSID, signal, channel, standard), ping, public and local IP, session traffic |
| **Battery** | charge, standing up; green while charging, red at 20% or less | health, cycles, capacity, charge or discharge power, time remaining |
| **Temperature** | a thermometer that rises and changes colour: the theme colour up to 70°, then yellow, orange, and red from 90° | each core, motherboard sensors (desktop) or board sensors (Dell), GPU, 24 h history |
| **Fans** | the fastest one, in RPM | each fan (motherboard headers on desktops, Dell laptops, GPU), history, the BIOS thermal mode (Dell) |
| **Power** | CPU power draw in watts | power by component (RAPL), GPU (NVIDIA), battery, 24 h history |

Also:

- **Light and dark themes**, following Windows or fixed, with both graph colours configurable.
- **24 hours of history** as one-minute averages, kept across restarts (Now / 1 h / 24 h on every graph).
- **Position** next to the Widgets button (between the weather and Start) or next to the tray, on a chosen monitor or on all of them.
- **Labels your way**: above, below, beside or none, with a live preview in Settings. The graphics can be turned off, leaving the text alone.
- **Compact mode**: two items per column, numbers only, tagged by icon or label, on the left or the right. Takes less than half the space.
- **Item order** set by dragging in Settings.
- **English and Portuguese**, following the Windows language or chosen in Settings; the installer is bilingual too.
- **Save memory**: unloads the panels after a minute unused, and wStats drops to about 18 MB (with the panels always ready it sits around 110 MB, almost all of it WebView2).
- **Start with Windows**, with a menu in the tray and on the taskbar's right click.

## How it works

```
 explorer.exe (taskbar)                             wstats.exe
┌──────────────────────────────────┐    shared    ┌──────────────────────────────┐
│ wStatsTAP.dll  (stub, fixed)     │    memory    │ collectors: PDH, IP Helper,  │
│   └─ wStatsCore.dll (swappable)  │◄────────────►│ WLAN, DXGI/D3DKMT, battery…  │
│        XAML element in the bar   │   pixels +   │ draws the strip (Direct2D)   │
│        clicks → wStats           │    clicks    │ panels in WebView2 (Tauri)   │
└──────────────────────────────────┘              └──────────────▲───────────────┘
                                                                 │ JSON, read-only
                                                  ┌──────────────┴───────────────┐
                                                  │ wstats-sensors.exe (service) │
                                                  │ CPU, Super I/O (PawnIO), Dell│
                                                  └──────────────────────────────┘
```

- **Taskbar integration.** The Windows 11 taskbar is XAML. wStats uses the official XAML diagnostics API (the one TranslucentTB uses) to load a small stub into Explorer. The stub loads the real code, which inserts an element into the taskbar's layout and adjusts the margins so the icons make room. The stub never changes, so updating wStats swaps the code inside Explorer **without restarting it**. Each taskbar (one per monitor) runs on its own thread, with its own timer.
- **Drawing.** wStats draws the strip with Direct2D and DirectWrite and publishes the pixels in shared memory. Explorer only shows the image and sends the clicks back.
- **Fallback.** If injection is not possible, wStats uses a window fitted into the taskbar's free space, measured with UI Automation, and tries to get back to XAML from time to time.
- **No admin.** The app runs as a normal user. Only CPU temperature and power, and fans, need privileges, and a separate service handles them (below).

### Sensor service

CPU temperature and power, and fans, cannot be read without privileges. Instead of running all of wStats as administrator, a small service (`wstats-sensors.exe`, ~220 KB) runs as the system and only publishes readings:

- **Intel CPUs:** temperature of each core and of the package (thermal MSRs).
- **AMD CPUs (Zen, family 17h–1Ah):** Tctl and each CCD's temperature.
- **CPU power (Intel and AMD):** RAPL energy counters, when Windows does not publish them itself (desktops, and every AMD CPU).
- **Desktop motherboards:** fans and board temperatures from the Super I/O chip: Nuvoton NCT6779D and newer (ASUS, ASRock, many MSI), NCT6683D/6686D/6687D (recent MSI) and ITE IT86xx/IT87xx (Gigabyte). Read only; fan control stays with the BIOS. Auxiliary temperature inputs the BIOS does not use are hidden, since unconnected pins read garbage.
- **Dell:** fans and board sensors through Dell's official WMI interface (DDV), and the BIOS thermal mode.

CPU and Super I/O reads go through [PawnIO](https://github.com/namazso/PawnIO), a signed driver that only runs signed modules. It is the same one LibreHardwareMonitor and FanControl use. None of wStats runs in the kernel, and Super I/O access holds the same lock other monitors use, so they can run side by side.

The service is turned on in **Settings › Temperature** (or › Fans) with one click and one UAC prompt. It is copied to `Program Files` before being registered, and it publishes its data in a memory section any user can read but only the system can write.

## Installation

Download the installer from [Releases](https://github.com/rbferreira/wStats/releases) and run it (or build your own with `npm run installer`). A single administrator prompt installs:

- wStats in `Program Files\wStats`, starting with Windows;
- if you accept (the default), the **sensor service** and, if it is not there yet, the **PawnIO driver**, using the official signed setup bundled in the installer.

Uninstalling removes the service as well. PawnIO stays unless you ask, since other programs may use it.

The installer is not code-signed yet: on first run, SmartScreen may warn you. Click "More info", then "Run anyway".

## Development

**Requirements:** Windows 11, [Visual Studio Build Tools 2022](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (C++), stable [Rust](https://rustup.rs) and Node.js 20+.

```bash
npm install
npx tauri dev                    # app in development mode
npx tauri build --no-bundle      # release build (src-tauri/target/release/wstats.exe)

cd sensors && cargo build --release   # sensor service

npm run installer                # NSIS installer in src-tauri/target/release/bundle/nsis
```

`npm run installer` builds the service, downloads the PawnIO setup (pinned version, checked by SHA-256 and signature) and packs everything with the hooks in `src-tauri/installer/hooks.nsh`.

`build.rs` builds the Explorer DLLs (`tap/`) automatically. In debug builds, rebuilding `tap/` swaps the code inside Explorer on the spot.

Tests: `cargo test` in `src-tauri/` and in `sensors/`, and `npx tsc --noEmit` for the front end.

Other tools:

- `preview.html` shows every panel with mock data in the browser (`npx vite`, then `/preview.html`; `?lang=en|pt`, `?theme=dark|light`).
- `WSTATS_STRIP=window` forces window mode, to test the fallback.
- `WSTATS_SHOTS=<folder> cargo test -- --ignored render_every_label` renders the strip in every label mode, dark and light, as PNGs.

### Layout

```
src/                 panels and Settings (TypeScript, no framework)
  modules/           one file per item: cpu, gpu, npu, mem, disk, net, battery, temp, fans, power
src-tauri/src/
  stats/             collectors and history (PDH, IP Helper, WLAN, DXGI, D3DKMT, battery, sensors)
  strip/             the taskbar strip: drawing, XAML integration and fallback
  popup.rs           panel placement and lifecycle
tap/                 DLLs that run inside Explorer (C++/WinRT)
sensors/             sensor service (Rust)
docs/                design reference and README images
```

## Credits

- Inspired by [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor).
- Taskbar integration based on [TranslucentTB](https://github.com/TranslucentTB/TranslucentTB)'s technique.
- [PawnIO](https://github.com/namazso/PawnIO) (GPL-2.0, official setup redistributed unmodified) and its [modules](https://github.com/namazso/PawnIO.Modules) (LGPL-2.1, see `sensors/pawnio/COPYING`), by namazso.
- Dell sensor data layout as in Linux's `dell-wmi-ddv` driver.

## License

[MIT](LICENSE).
