# CalLink

A cross-platform Rust reimplementation of the core of **TI Connect CE 6.1**: talking to
TI-84 Plus / TI-84 Plus CE family calculators over USB, and reading and writing `.8x*` files.

## What TI Connect CE is, internally

From `TIConnectCE-6.1.0.712.exe`:

| Layer | Original | Here |
|---|---|---|
| Installer | Advanced Installer bootstrapper → MSI + CAB (first 512 bytes XOR 0xFF) | — |
| App | jpackage'd Java 17 + JavaFX (`ticonnect.jar`): Calculator Explorer, Screen Capture, Program Editor, CSV import, OS update | `src/main.rs` CLI |
| USB | `NativeConnectServer.dll` (JNI, C) using WinUSB; protocol called "CARS" (DUSB) | `src/usb.rs` (libusb) + `src/dusb.rs` |
| Program text ↔ tokens | a token dictionary + `Translator` JNI calls | `data/tokens.tsv` (CalLink's own table) + `src/tokens.rs` |
| Var files | `TIVarImpl`, `VarDataParser` (`**TI83F*`) | `src/varfile.rs` |

## Supported calculators

CalLink identifies the model from its product ID (parameter 0x0001) and name, and adapts:

| Model | ID | Screenshot | OS file | Tested on hardware |
|---|---|---|---|---|
| TI-84 Plus, TI-84 Plus Silver Edition | 0x0A | 96×64 mono | .8xu | ✓ everything incl. OS install |
| TI-84 Plus CE (also CE-T, CE Python editions) | 0x13 | 320×240 colour | .8eu | ✓ everything except restore and OS install |
| TI-84 Plus C Silver Edition | 0x0F | 320×240, RLE-compressed (decoded) | .8cu | simulator only |
| TI-83 Premium CE | 0x13 | 320×240 colour | .8pu | simulator only |
| TI-84 Plus T | 0x1B | 96×64 mono | .8xu | simulator only |
| TI-82 Advanced | 0x0B | 96×64 mono | .82u | simulator only |
| Others (e.g. TI-84 Evo, 2026) | — | — | — | not supported knowingly; CalLink tries any unrecognised TI USB device |

Per model: version numbers are shown the way TI writes them (2.55, 4.20, 5.8.3); pictures save
as .8xi on monochrome models and .8ci on colour ones; the debugger simulates a 16×8 or 26×10 home
screen; the clock uses seconds-since-1997 or separate date/time fields, whichever the calculator
answers; OS updates check TI's page for the model (TI-84 Plus, C SE, CE, CE Python, CE-T Python;
TI publishes no page CalLink knows for the 83 Premium CE, 84 Plus T or 82 Advanced) and install
via the Z80 page path or the CE flat path. **Before installing, CalLink checks the model ID in the
OS file's header (`80 1x`) against the calculator** and refuses a mismatch, for example a
TI-84 Plus .8xu on a C SE or an 84 Plus T, which all look alike as files. Model details come from
the libticalcs sources; the C SE screenshot decoder is tested on synthetic data only.

## Install

CalLink runs on **Linux, Windows and macOS**. Builds come from GitHub Actions
(`.github/workflows/build.yml`: start it from the Actions tab, or push a `v*` tag to publish a
release with all three).

### Windows (10 and 11)

Unzip `CalLink-<version>-windows-x64.zip` and run `callink.exe` (the GUI) or
`callink-cli.exe` (command line). **USB driver:** Windows only lets CalLink use a calculator
with a WinUSB driver. Installing TI Connect CE once provides TI's driver (keep TI Connect CE
closed while CalLink runs: only one program can use the calculator); without it, use
[Zadig](https://zadig.akeo.ie) to install WinUSB for the TI device. See `USB-DRIVER.txt` in the zip.
The executables aren't code-signed, so SmartScreen may ask you to confirm the first run
(More info → Run anyway).

### macOS (11 Big Sur and later, Apple Silicon and Intel)

Open `CalLink-<version>-macos.dmg` and drag CalLink to Applications. No USB driver is needed.
The app is not signed with an Apple Developer ID, so the first time, right-click it and choose
**Open** (or allow it under System Settings → Privacy & Security). The command-line tool is
inside the app: `/Applications/CalLink.app/Contents/MacOS/callink-cli`.

### Linux (AppImage)

```
./packaging/build-appimage.sh            # → dist/CalLink-<version>-x86_64.AppImage
./dist/CalLink-0.1.0-x86_64.AppImage     # GUI
./dist/CalLink-0.1.0-x86_64.AppImage --cli list   # command-line tool
```

The script needs only `cargo` and `mksquashfs`. `packaging/runtime-x86_64` is the AppImage
project's static type-2 runtime (MIT), with its update-info and signature sections blanked. It
needs no libfuse2. An AppImage can't install udev rules, so on a new machine add the rule under
"Linux permissions" once. `packaging/make_icon.py` regenerates `packaging/callink.png` and `.ico`.

### Building from source

Any OS with Rust: `cargo build --release` gives `target/release/callink` (GUI) and
`callink-cli`. Packaging: `packaging/build-appimage.sh` (Linux), `packaging/build-windows.ps1`
(Windows), `packaging/build-macos.sh` (macOS, universal binary).

## GUI

```
cargo run --release --bin callink
```

Three tabs, like TI Connect CE:

* **Calculator Explorer**: device info; **Back up calculator** (every variable into a new
  dated folder of ordinary `.8x*` files plus a `backup.txt` manifest), **Restore backup**
  (sends a backup folder back, replacing same-named variables after asking), and
  **Sync date & time** (sets the calculator clock to this computer's local time), and
  **OS update** (checks TI's website for a newer OS for this model, opens TI's download page,
  and installs a downloaded `.8xu`/`.8eu` with a progress bar); variable list, send (button or drag-and-drop `.8x*`
  files onto the window), receive to a folder, delete, and double-click to edit a program
  or view a variable's contents.
* **Screen Capture**: take a screenshot, zoom 1–4×, save as PNG.
* **Simulator**: a TI-84 Plus CE-style calculator built by CalLink (not an emulator of TI's
  hardware or OS). Clickable CE keypad with every key's 2nd/alpha function and real getKey code,
  or type on your keyboard; colour home screen with entry history, Ans, variables, 2nd ENTRY and
  error screens (Quit/Goto, which opens the Program Editor at the line); PRGM (EXEC/CTL/I/O),
  MATH/NUM/PROB, TEST/LOGIC and CATALOG menus; running programs with `prgmNAME` (programs can
  call each other), Input/Prompt/Pause/Menu(/getKey and ON to break; Y= editor and a graph screen
  (arrows pan, zoom resets). The program open in the editor is always loaded; more can be added
  from .8xp files. TI's own menus, apps, and graph-screen drawing commands aren't simulated.
* **Program Editor**: **suggestions as you type** from the token catalog (↑↓ choose, Tab/Enter
  insert, Esc close), each with a short description; ranking is predictive (tokens already used
  in the program, common tokens, and commands at the start of a statement vs functions inside
  expressions come first); ASCII shortcuts suggest symbols (`sqrt`→`√(`, `->`→`→`, `<=`→`≤`,
  `!=`→`≠`, `L1`→`L₁`, `pi`→`π`, `theta`→`θ`, `neg`→`⁻`…); inside a call such as `randInt(` or
  `Output(` a hint shows the arguments and highlights the one being typed. A **Problems** panel that checks the program as you type (missing or extra
  End, Then/Else without If, Goto/Menu( to a missing Lbl, duplicate and unused labels, Goto
  jumping out of loops, subtract used as a negative sign, bad tokens…; click one to jump to it);
  a **step-through debugger** (Run/Continue F5, Step F10, Pause, Stop Shift+F5) that runs the
  program on a simulated CE 26×10 or TI-84 Plus 16×8 home screen with breakpoints (click a line
  number), current-line highlighting, a variables view, Input/Prompt/Pause/Menu( handling and
  keyboard → getKey. It interprets a TI-BASIC subset (reals, strings, lists, If/For(/While/Repeat,
  Goto/Lbl, IS>(/DS<(, common math, list and string functions) and reports the calculator's error
  names; graph-screen commands are skipped and listed; matrices, complex numbers and calling other
  programs stop with a clear message. Also: open/save `.8xp` or `.txt`, live token check (byte count, or the
  line/column that fails), searchable token catalog that inserts at the cursor, and send
  straight to the calculator.

USB work runs on a background thread, so the window stays responsive. To try the GUI without
a calculator, run it with `CALLINK_MOCK=1` to use a simulated TI-84 Plus CE with sample variables.
`cargo build --no-default-features` builds only the CLI.

## Commands

```
callink-cli devices                  # list TI USB devices
callink-cli info                     # model, OS version, memory, clock…
callink-cli list                     # variables on the calculator
callink-cli get HELLO L₁ -o out/     # receive variables → .8xp/.8xl/…
callink-cli get --all -o backup/
callink-cli send GAME.8xp DATA.8xv [--archive|--ram]
callink-cli delete HELLO [-t program]
callink-cli screenshot -o screen.png [--scale 2]
callink-cli file info|show FILE.8x?  # inspect files offline
callink-cli prog decode X.8xp [-o x.txt]
callink-cli prog encode x.txt [-n NAME] [-o X.8xp] [--protected]
callink-cli backup -o ~/Backups              # every variable → new dated folder
callink-cli restore "~/Backups/CalLink backup …"   # send a backup back (asks first; -y to skip)
callink-cli sync-clock                        # set the calculator clock to local time
callink-cli check-update                      # newer OS on TI's site?
callink-cli install-os TI84Plus_OS255.8Xu     # install a downloaded OS (asks first)
callink-cli tokens                   # dump the token table
```

`-v` prints every raw/virtual packet.

### Linux permissions

```
printf '%s\n' 'SUBSYSTEM=="usb", ATTR{idVendor}=="0451", ATTR{idProduct}=="e003", MODE="0666"' 'SUBSYSTEM=="usb", ATTR{idVendor}=="0451", ATTR{idProduct}=="e008", MODE="0666"' | sudo tee /etc/udev/rules.d/69-ticalc.rules
sudo udevadm control --reload && sudo udevadm trigger
```

On Windows, close TI Connect CE first; the TI driver it installs is WinUSB and libusb works with it.

## Token text conventions

* Readable Unicode instead of TI's private-use font glyphs (`ᴇ`, `⁻¹`, `►`, `𝑖`, `ℯ`, `ᵀ`, `ʳ`, `Δ`, `ᴛ`, `³`, `₁₀`),
  `⁻` for the negative sign (TI's table uses an invisible soft hyphen) and `ʟ` for custom lists (TI's uses `⌊`).
* Command words whose token includes a trailing space (`Disp `, `Pause `, `Input `…) are also
  recognised without it at the end of a line or before `:`.
  Both spellings are accepted when encoding; `--raw-glyphs` keeps TI's.
* Lowercase `a`–`z` are the lowercase-letter tokens. The statistics and sequence variables
  that TI's table spells as single letters are written `|a`, `|n`, `|u`, … instead.
* Tokens with no text form come out as `\x{HHHH}` and are accepted back.
* Decoding and then encoding a program gives back the original bytes; every token in the table is tested.
* `data/tokens.tsv` is CalLink's table of token codes and their text (see its header).

## Status

**Tested on a real TI-84 Plus (OS 2.55MP, boot 1.03)**: info, list, get (including a
6.5 KB appvar spread over many packets), send, delete and screenshot all work, from both the CLI
and the GUI. A program sent and read back came out byte-for-byte identical.

What the hardware showed that the documentation didn't:
* The reply to the opening ping is `0x0012` (mode set), not a data ACK.
* Variable names travel over USB as UTF-8 text (`L₁`, `Str1`); .8x files hold tokens
  (`5D 00`), so names are converted both ways.
* Right after a directory listing ends, the calculator silently drops the next request. The
  tool waits 250 ms after each EOT.
* Parameters `0x0003` and `0x000F` get no answer and wedge the link. They're left out of
  `info`, and any hang is now recovered with a USB reset and reconnect.
* Error `0x0006` means "variable not found".

**Also verified on a real TI-84 Plus CE (OS 5.8.3, boot 5.6.1)**: info, list, get (a 22 KB
image in under a second, with 1018-byte packets), backup (42 variables in 2 s), screenshot (true
colour; RGB565 little-endian as assumed), send, delete, clock sync and the update check. What the CE
showed:
* Versions are `00 major minor patch` (5.8.3), unlike the TI-84 Plus's `00 2 55 0`.
* It sometimes reports free RAM as 0; that line is hidden then.
* It refuses the seconds-since-1997 clock parameter; the date and time are separate parameters
  (0x003B–0x0040). In 12-hour mode it stores the hour modulo 12 and reports it without AM/PM, so
  sync switches to 24-hour mode, sets and checks the time, then restores the user's setting.
* Image variables are named `3C 00`…`3C 09` in files (Image1…Image0), not by the `Image1` token.
* Delete needs the reference MODIF_VAR layout (attributes, then the protection byte, then an empty
  destination). An earlier layout that the TI-84 Plus accepted got ERROR 0x0004. The new layout
  hasn't been re-run on the TI-84 Plus yet.

Not tested on the CE: restore, and OS install (5.8.5 is available for it).
`examples/probe.rs` sends a single hand-written packet, for poking at a new model.

Backup and clock sync are also verified on the TI-84 Plus. Restore so far is tested only against
the simulator. Backups leave out flash apps and the OS, which can't be read back over USB.

OS install follows libticalcs' `send_os` / `send_os_834pce` sequence exactly. **Verified on a
real TI-84 Plus**: reinstalling OS 2.55 (`TI84Plus_OS255.8Xu`, 1,410 data packets of 256 bytes,
about 2.5 minutes) succeeded; RAM was cleared and the archive and apps were kept. Don't talk to the
calculator for about a minute afterwards while it verifies the OS: probing it then knocked it off
USB until the cable was replugged, so the GUI keeps Connect disabled for 60 s. TI's download
of that file has ~300 bytes of HTML appended after the OS data and a `^Z` after the last record;
both are handled. The CE install is still tested only against the simulator. The update
check reads the version from the OS file link on TI's model page (verified live).

Not implemented: sending/receiving flash apps (`.8ek`/`.8xk`),
CSV→list import, TI-84 Plus C SE compressed screenshots.

## License

CalLink is licensed under the MIT license ([LICENSE](LICENSE)).
`packaging/runtime-x86_64` is the AppImage project's type-2 runtime (MIT); see
[packaging/THIRD-PARTY.md](packaging/THIRD-PARTY.md).

CalLink is an independent project, not made by or affiliated with Texas Instruments. "TI",
"TI-84 Plus", "TI Connect" and related names are trademarks of Texas Instruments, used here
only to describe compatibility.
