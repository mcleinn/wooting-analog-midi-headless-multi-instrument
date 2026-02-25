# Headless (No-GUI) Wooting Analog MIDI

This repository primarily contains a GUI app (Tauri + React). For Raspberry Pi / servers you can run a headless daemon that creates a virtual ALSA MIDI output port and streams MIDI from one or more Wooting analog devices.

## Prereqs

1) Wooting Analog SDK

- `libwooting_analog_sdk.so` in a library path (e.g. `/usr/lib/libwooting_analog_sdk.so`)
- Plugins installed in the default plugin directory:
  - `/usr/local/share/WootingAnalogPlugins/wooting-analog-plugin/libwooting_analog_plugin.so`

2) ALSA sequencer

- `/dev/snd/seq` must exist (kernel module `snd_seq`)

3) Build deps (Debian/Ubuntu)

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libasound2-dev
```

## Build

```bash
cargo build --release --manifest-path wooting-analog-midi-core/Cargo.toml --bin wooting-analog-midi-headless
```

Binary output:

- `wooting-analog-midi-core/target/release/wooting-analog-midi-headless`

## Run

List connected devices (prints `device_id`):

```bash
./wooting-analog-midi-core/target/release/wooting-analog-midi-headless --list-devices
```

Run the daemon (creates config if missing):

```bash
RUST_LOG=info ./wooting-analog-midi-core/target/release/wooting-analog-midi-headless
```

Default config path:

- `~/.config/wooting-midi/headless.json`

Default virtual port name:

- `Wooting Analog MIDI`

## MODEP (Patchbox)

On Patchbox/MODEP with `jackd -X seq`, the daemon creates an ALSA sequencer output port typed as `HARDWARE`. This makes the bridged JACK MIDI port appear as `physical`, so it shows up in MODEP's separated MIDI device list.

## Config (Per-Device Channel)

Each device can be pinned to exactly one MIDI channel (0-15). The `device_id` comes from `--list-devices`.

See `contrib/headless.json` for an example.

## systemd

See `contrib/systemd/wooting-analog-midi-headless.service`.
