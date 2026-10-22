# UI framework decision: GPUI

**Decision (2026-10-23):** GPUI (`gpui` 0.2.2 from crates.io, Apache-2.0).

We built the same mini-launcher twice, in GPUI and in Slint 1.18, and measured both with an identical self-test. The mini-launcher has:
- a hotkey and a prewarmed frameless window;
- text input and a list of installed apps with fuzzy filtering;
- Enter to launch.

Both met every latency target. GPUI was chosen for the fastest rendering and the Apache-2.0 licence. The cost is a higher idle memory footprint than Slint's software renderer, which we accepted.

The Slint spike isn't part of this repository; its results are summarised below.

## Targets

| Metric | Target |
|---|---|
| Hotkey → window visible | < 50 ms |
| Keystroke → list updated (10k items) | < 16 ms |
| Idle RAM, hidden | ≤ 120 MB private (revised from 60 MB after measuring GPUI's floor) |
| IME, blur, rounded corners, 125–150% scaling | work (manual check) |

## Method

`SPIKE_SELFTEST=1` runs 15 show/hide cycles, then types an 18-character query one character at a time. GPUI's self-test then also sends Ctrl+Backspace through GPUI's key dispatch. Finally it records memory after 5 s hidden.

Latency is measured from the trigger to the **paint phase** of the frame that shows the change. Only Present and DWM composition (up to one vsync) come after that.

The self-test injects presses into the same channel the hotkey thread uses and types through the IME entry point. OS hotkey dispatch and key-event delivery are therefore not included.

Machine: Windows 11 Pro (build 26300), NVIDIA RTX 4060, Rust 1.99.1 MSVC, release build.

## Results

GPUI, current spike (680 × 420 window, 176 apps + 10,000 synthetic rows):

| Metric (median / p95) | Result |
|---|---|
| Warm hotkey → painted | 5.0 / 5.4 ms |
| First show → painted | ~13 ms |
| Keystroke → painted | 2.7–3.4 / ~5–6 ms |
| Fuzzy filter per keystroke (10k rows) | 0.36 ms |
| Idle RAM, hidden (private) | ~71–75 MB |
| Release binary | 11.1 MB |

Comparison run, done before the window-size fix described below:

| Metric (median / p95) | GPUI 0.2.2 | Slint femtovg (OpenGL) | Slint software |
|---|---|---|---|
| Warm hotkey → painted, +10k rows | **5.1 / 6.3 ms** | 8.3 / 10.9 ms | 9.3 / 10.4 ms¹ |
| Keystroke → painted, +10k rows | **2.8 / 5.8 ms** | 5.6 / 8.2 ms | 5.6 / 6.0 ms¹ |
| Idle RAM hidden, private | ~104 MB² | 112–175 MB | **8–12 MB** |
| Threads at idle | 41 | 31 | 12 |

¹ The software renderer has no rendering notifier, so its latency is measured after Present, which is slightly pessimistic.
² Measured with an accidental 1920 × 1023 window (see below). With the correct size, GPUI is ~71–75 MB.

**Why GPUI despite Slint software's memory:**
- **Speed:** both meet the targets, and GPUI is the fastest.
- **Licence:** Apache-2.0. Slint is GPLv3, a royalty-free licence with attribution, or commercial.
- **Text rendering** built for an editor.

The trade-off is more UI code to write ourselves: text input and list rows. Our own widget set starts from `crates/ui/src/text_input.rs`.

## Findings worth keeping

- **Window size with hidden creation.** With `show: false`, GPUI applies the window size in `activate()`. The spike shows the window through Win32, which bypasses that, so the size must be set explicitly with the monitor's DPI. Before the fix the window was 1920 × 1023; fixing it also cut idle RAM by ~30 MB.
- **RAM floor.** A bare GPUI window with one label (`cargo run -p spotlight --example baseline --release`) uses ~71 MB private on the test machine. GPU driver and device overhead is the likely bulk (unverified).
- **Background.** GPUI's `Blurred` mode (accent policy) barely blurs on Windows 11, so a dense tint is used. The DWM acrylic system backdrop rendered light grey and lost focus after a few seconds.
- **Alt+Space** can be held by another app. The real app needs a hotkey picker.

## Still to verify by hand

- IME composition and candidate-window position (Japanese / Chinese).
- Text sharpness at 125% / 150% scaling.
- The invisible border around the window: the visible area is ~8 px narrower than the 680 × 420 we request.
