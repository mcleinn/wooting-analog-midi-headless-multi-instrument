use alsa::seq::{EvNote, Event, EventType, PortCap, PortType, Seq};
use alsa::Direction;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use wooting_analog_midi_core::{
    FromPrimitive, HIDCodes, MidiEngine, NoteConfig, NoteID, NoteSink, WootingAnalogResult,
    REFRESH_RATE,
};

use wooting_analog_wrapper as sdk;

struct AlsaSeqOut {
    seq: Seq,
    port: i32,
}

impl AlsaSeqOut {
    fn new(client_and_port_name: &str) -> Result<Self> {
        let seq = Seq::open(None, Some(Direction::Playback), false)
            .context("Failed to open ALSA sequencer")?;

        let cname = CString::new(client_and_port_name)
            .context("Invalid ALSA client name (contains NUL)")?;
        seq.set_client_name(cname.as_c_str())
            .context("Failed to set ALSA sequencer client name")?;

        let pname =
            CString::new(client_and_port_name).context("Invalid ALSA port name (contains NUL)")?;

        // Mark this as HARDWARE so jackd -X seq exposes it as a 'physical'
        // system:midi_capture_* port, which MODEP includes in separated mode.
        let caps = PortCap::READ | PortCap::SUBS_READ;
        let typ = PortType::MIDI_GENERIC | PortType::HARDWARE | PortType::APPLICATION;
        let port = seq
            .create_simple_port(pname.as_c_str(), caps, typ)
            .context("Failed to create ALSA sequencer port")?;

        Ok(Self { seq, port })
    }

    fn send_evnote(&mut self, t: EventType, note: u8, value: u8, channel: u8) -> Result<()> {
        let ev = EvNote {
            channel,
            note,
            velocity: value,
            off_velocity: 0,
            duration: 0,
        };
        let mut e = Event::new(t, &ev);
        e.set_source(self.port);
        e.set_subs();
        e.set_direct();
        self.seq
            .event_output_direct(&mut e)
            .context("Failed to output ALSA sequencer event")?;
        Ok(())
    }
}

impl NoteSink for AlsaSeqOut {
    fn note_on(&mut self, note_id: NoteID, velocity: f32, channel: u8) -> Result<()> {
        let vbyte = (f32::min(velocity, 1.0) * 127.0) as u8;
        self.send_evnote(EventType::Noteon, note_id, vbyte, channel)
    }

    fn note_off(&mut self, note_id: NoteID, velocity: f32, channel: u8) -> Result<()> {
        let vbyte = (f32::min(velocity, 1.0) * 127.0) as u8;
        self.send_evnote(EventType::Noteoff, note_id, vbyte, channel)
    }

    fn polyphonic_aftertouch(&mut self, note_id: NoteID, pressure: f32, channel: u8) -> Result<()> {
        let pbyte = (f32::min(pressure, 1.0) * 127.0) as u8;
        self.send_evnote(EventType::Keypress, note_id, pbyte, channel)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Config {
    #[serde(default = "default_virtual_port_name")]
    virtual_port_name: String,

    #[serde(default)]
    devices: Vec<DeviceConfig>,

    #[serde(default)]
    default: DefaultConfig,

    #[serde(default = "default_refresh_hz")]
    refresh_hz: f32,

    #[serde(default = "default_max_items")]
    max_items: usize,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct DefaultConfig {
    #[serde(default = "default_shift_amount")]
    shift_amount: i8,

    #[serde(default)]
    note_config: NoteConfig,

    #[serde(default)]
    mapping: Vec<KeyMapping>,
}

impl Default for DefaultConfig {
    fn default() -> Self {
        Self {
            shift_amount: default_shift_amount(),
            note_config: NoteConfig::default(),
            mapping: default_mapping(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct DeviceConfig {
    device_id: u64,
    /// 0-15
    channel: u8,

    #[serde(default)]
    mapping: Option<Vec<KeyMapping>>,

    #[serde(default)]
    shift_amount: Option<i8>,

    #[serde(default)]
    note_config: Option<NoteConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct KeyMapping {
    /// HID key code, e.g. 0x04 for A
    key: u8,
    /// MIDI note number
    note: NoteID,
}

fn default_virtual_port_name() -> String {
    "Wooting Analog MIDI".to_string()
}

fn default_shift_amount() -> i8 {
    12
}

fn default_refresh_hz() -> f32 {
    REFRESH_RATE
}

fn default_max_items() -> usize {
    256
}

fn default_mapping() -> Vec<KeyMapping> {
    vec![
        KeyMapping {
            key: HIDCodes::A as u8,
            note: 57,
        },
        KeyMapping {
            key: HIDCodes::W as u8,
            note: 58,
        },
        KeyMapping {
            key: HIDCodes::S as u8,
            note: 59,
        },
        KeyMapping {
            key: HIDCodes::D as u8,
            note: 60,
        },
        KeyMapping {
            key: HIDCodes::R as u8,
            note: 61,
        },
        KeyMapping {
            key: HIDCodes::F as u8,
            note: 62,
        },
        KeyMapping {
            key: HIDCodes::T as u8,
            note: 63,
        },
        KeyMapping {
            key: HIDCodes::G as u8,
            note: 64,
        },
        KeyMapping {
            key: HIDCodes::H as u8,
            note: 65,
        },
        KeyMapping {
            key: HIDCodes::U as u8,
            note: 66,
        },
    ]
}

fn default_config_path() -> Result<PathBuf> {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        let home = std::env::var("HOME").context("HOME is not set")?;
        Path::new(&home).join(".config")
    };
    Ok(base.join("wooting-midi").join("headless.json"))
}

fn load_or_create_config(path: &Path) -> Result<Config> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config dir {parent:?}"))?;
    }

    if !path.exists() {
        let cfg = Config {
            virtual_port_name: default_virtual_port_name(),
            devices: vec![],
            default: DefaultConfig::default(),
            refresh_hz: default_refresh_hz(),
            max_items: default_max_items(),
        };
        let content =
            serde_json::to_string_pretty(&cfg).context("Failed to serialize default config")?;
        fs::write(path, content.as_bytes())
            .with_context(|| format!("Failed to write default config to {path:?}"))?;
        return Ok(cfg);
    }

    let content = fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;
    let cfg: Config = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON config {path:?}"))?;
    Ok(cfg)
}

fn mapping_vec_to_map(
    mapping: &[KeyMapping],
    channel: u8,
) -> Result<HashMap<HIDCodes, Vec<(u8, NoteID)>>> {
    if channel > 15 {
        anyhow::bail!("Invalid channel {channel}, must be 0-15");
    }

    let mut out: HashMap<HIDCodes, Vec<(u8, NoteID)>> = HashMap::new();
    for m in mapping.iter() {
        let hid = HIDCodes::from_u8(m.key)
            .ok_or_else(|| anyhow::anyhow!("Invalid HID key code: {}", m.key))?;
        out.entry(hid).or_default().push((channel, m.note));
    }
    Ok(out)
}

fn list_devices() -> Result<()> {
    let device_num = sdk::initialise().0?;
    println!("SDK initialised, devices reported: {device_num}");

    let devices = sdk::get_connected_devices_info(32).0?;
    for (i, d) in devices.iter().enumerate() {
        println!(
            "[{i}] device_id={} vid=0x{:04x} pid=0x{:04x} manufacturer=\"{}\" name=\"{}\"",
            d.device_id, d.vendor_id, d.product_id, d.manufacturer_name, d.device_name
        );
    }

    sdk::uninitialise();
    Ok(())
}

fn main() -> Result<()> {
    env_logger::init();

    let mut args = std::env::args().skip(1);
    let mut config_path: Option<PathBuf> = None;
    let mut override_port_name: Option<String> = None;
    let mut do_list_devices = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                let p = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--config requires a path"))?;
                config_path = Some(PathBuf::from(p));
            }
            "--port-name" => {
                let n = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port-name requires a value"))?;
                override_port_name = Some(n);
            }
            "--list-devices" => {
                do_list_devices = true;
            }
            "-h" | "--help" => {
                println!(
                    "Usage: wooting-analog-midi-headless [--config PATH] [--port-name NAME] [--list-devices]\n\n\
Creates a single virtual ALSA MIDI output port and outputs MIDI from one or more Wooting analog devices.\n\n\
Default config path: ~/.config/wooting-midi/headless.json"
                );
                return Ok(());
            }
            _ => {
                anyhow::bail!("Unknown argument: {a}");
            }
        }
    }

    if do_list_devices {
        return list_devices();
    }

    let config_path = config_path.unwrap_or(default_config_path()?);
    let mut cfg = load_or_create_config(&config_path)?;
    if let Some(n) = override_port_name {
        cfg.virtual_port_name = n;
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || {
            running.store(false, Ordering::SeqCst);
        })
        .context("Failed to install Ctrl-C handler")?;
    }

    // Start the SDK
    let _device_num = sdk::initialise()
        .0
        .context("Failed to initialise Wooting Analog SDK")?;

    // Create one ALSA sequencer output port (hardware-typed for MODEP)
    let mut conn_out = AlsaSeqOut::new(&cfg.virtual_port_name)?;

    // Build config lookup
    let mut configured: HashMap<u64, DeviceConfig> = HashMap::new();
    for d in cfg.devices.iter() {
        configured.insert(d.device_id, d.clone());
    }

    // Engines per device
    let mut engines: HashMap<u64, MidiEngine> = HashMap::new();
    let mut engine_channels: HashMap<u64, u8> = HashMap::new();

    let mut last_device_scan = Instant::now() - Duration::from_secs(999);

    while running.load(Ordering::SeqCst) {
        if last_device_scan.elapsed() >= Duration::from_secs(2) {
            last_device_scan = Instant::now();
            let devices = sdk::get_connected_devices_info(32).0.unwrap_or_default();

            let connected_ids: HashSet<u64> = devices.iter().map(|d| d.device_id).collect();

            // Remove engines for disconnected devices
            let existing_ids: Vec<u64> = engines.keys().cloned().collect();
            for id in existing_ids {
                if !connected_ids.contains(&id) {
                    if let Some(mut e) = engines.remove(&id) {
                        let _ = e.all_notes_off(&mut conn_out);
                    }
                    engine_channels.remove(&id);
                }
            }

            // Add engines for newly connected devices
            for dev in devices.iter() {
                let id = dev.device_id;
                if engines.contains_key(&id) {
                    continue;
                }

                let (channel, mapping, shift_amount, note_config) = if let Some(dc) =
                    configured.get(&id)
                {
                    let map_vec = dc
                        .mapping
                        .clone()
                        .unwrap_or_else(|| cfg.default.mapping.clone());
                    let shift = dc.shift_amount.unwrap_or(cfg.default.shift_amount);
                    let ncfg = dc
                        .note_config
                        .clone()
                        .unwrap_or_else(|| cfg.default.note_config.clone());
                    (dc.channel, map_vec, shift, ncfg)
                } else {
                    // Auto-assign next free channel
                    let mut used: HashSet<u8> = configured.values().map(|d| d.channel).collect();
                    used.extend(engine_channels.values().copied());
                    let mut ch: u8 = 0;
                    while ch < 16 && used.contains(&ch) {
                        ch += 1;
                    }
                    if ch >= 16 {
                        // Fall back to channel 0 if exhausted
                        ch = 0;
                    }

                    eprintln!(
                        "Device {} ({} {}) not in config; auto-assigning channel {}",
                        dev.device_id, dev.manufacturer_name, dev.device_name, ch
                    );

                    (
                        ch,
                        cfg.default.mapping.clone(),
                        cfg.default.shift_amount,
                        cfg.default.note_config.clone(),
                    )
                };

                let mut engine = MidiEngine::new();
                engine.amount_to_shift = shift_amount;
                engine.set_note_config(note_config.clone());

                let map = mapping_vec_to_map(&mapping, channel)
                    .with_context(|| format!("Failed to build mapping for device {id}"))?;
                engine.update_mapping(&map);

                engine_channels.insert(id, channel);
                engines.insert(id, engine);
            }
        }

        // Poll each device and feed its engine
        let device_ids: Vec<u64> = engines.keys().cloned().collect();
        for id in device_ids {
            let data = match sdk::read_full_buffer_device(cfg.max_items, id).0 {
                Ok(d) => d,
                Err(e) => {
                    // Remove engine if device disappeared
                    if e == WootingAnalogResult::NoDevices
                        || e == WootingAnalogResult::DeviceDisconnected
                    {
                        if let Some(mut e) = engines.remove(&id) {
                            let _ = e.all_notes_off(&mut conn_out);
                        }
                    }
                    continue;
                }
            };

            if let Some(engine) = engines.get_mut(&id) {
                if let Err(e) = engine.process_analog(&data, &mut conn_out) {
                    eprintln!("Error processing device {id}: {e:#}");
                }
            }
        }

        let sleep_dur = Duration::from_secs_f32(1.0 / cfg.refresh_hz.max(1.0));
        thread::sleep(sleep_dur);
    }

    // Shutdown
    for engine in engines.values_mut() {
        let _ = engine.all_notes_off(&mut conn_out);
    }
    sdk::uninitialise();
    Ok(())
}
