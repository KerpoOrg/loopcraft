use super::*;

fn invalid(yaml: &str) -> Vec<String> {
    match parse(yaml) {
        Err(Error::Invalid(p)) => p,
        other => panic!("expected validation problems, got {other:?}"),
    }
}

fn syntax(yaml: &str) -> String {
    match parse(yaml) {
        Err(Error::Syntax(m)) => m,
        other => panic!("expected a syntax error, got {other:?}"),
    }
}

fn w(from: u32, to: u32) -> Wire {
    Wire { from, to }
}

#[test]
fn example_parses() {
    let c = parse(include_str!("../example.yaml")).unwrap();
    assert_eq!(c.devices.len(), 1);
    let d = &c.devices[0];
    assert_eq!((d.name.as_str(), d.uid().as_str()), ("Desktop", "org.kerpo.loopcraft:desktop"));
    assert_eq!((d.busses, d.virtual_inputs, d.sample_rate), (10, 2, 48_000));

    assert_eq!(d.inputs.len(), 4);
    assert_eq!(d.inputs[0].kind, InputKind::Virtual);
    assert_eq!(d.inputs[0].wires, vec![w(1, 1), w(2, 2)]);
    assert_eq!(d.inputs[1].kind, InputKind::Device("Rane FOUR".into()));
    assert_eq!(d.inputs[2].wires, vec![w(1, 3), w(1, 4)]);
    assert_eq!(d.inputs[3].kind, InputKind::App { name: "Spotify".into(), mute_original: true, tap: false });
    assert_eq!(d.inputs[3].wires, vec![w(1, 7), w(2, 8)]);
    assert!(d.inputs.iter().all(|s| s.enabled && !s.muted && s.gain_db == 0.0));

    assert_eq!(d.outputs.len(), 2);
    assert_eq!(d.outputs[0].wires, vec![w(9, 13), w(10, 14)]);
    assert_eq!((&d.outputs[1].kind, d.outputs[1].gain_db), (&OutputKind::Device("PRO X Wireless Gaming Headset".into()), -3.0));
}

#[test]
fn converts_to_plugin_specs() {
    let specs = parse(include_str!("../example.yaml")).unwrap().to_specs();
    assert_eq!(
        specs,
        vec![DeviceSpec {
            uid: "org.kerpo.loopcraft:desktop".into(),
            name: "Desktop".into(),
            channels: 10,
            pass_thru: 2,
            sample_rate: 48_000.0,
            apps: vec![],
            apply_gain: false, // the headset follows the device volume
            virtuals: vec![],
        }]
    );
    // Whatever the config accepts, the plug-in must accept too.
    assert_eq!(lc_spec::parse(&lc_spec::to_json(&specs)).unwrap(), specs);
}

#[test]
fn defaults_and_empty_files() {
    assert_eq!(parse("").unwrap(), Config::default());
    assert_eq!(parse("# nothing yet\n").unwrap(), Config::default());
    assert_eq!(parse("devices: []").unwrap(), Config::default());
    let c = parse("devices:\n  - {name: Rec, id: rec}\n").unwrap();
    let d = &c.devices[0];
    assert_eq!((d.busses, d.virtual_inputs, d.sample_rate, d.buffer_frames), (2, 2, 48_000, None));
    assert!(d.inputs.is_empty() && d.outputs.is_empty());
    assert_eq!(parse("devices:\n  - {name: Rec, id: rec, buffer_frames: 64}\n").unwrap().devices[0].buffer_frames, Some(64));
    assert_eq!(parse("devices:\n  - {name: Rec, id: rec, buses: 8}\n").unwrap().devices[0].busses, 8, "'buses' spelling");
}

#[test]
fn quoted_and_string_channels() {
    let c = parse("devices:\n  - name: A\n    id: a\n    inputs:\n      - virtual: true\n        wires: {'1': '2', \"2\": [1]}\n").unwrap();
    assert_eq!(c.devices[0].inputs[0].wires, vec![w(1, 2), w(2, 1)]);
}

#[test]
fn shape_errors_are_syntax_errors() {
    assert!(!syntax("devices: [").is_empty());
    // Typos in keys must not be silently ignored.
    assert!(syntax("devices:\n  - name: A\n    id: a\n    bussses: 4\n").contains("bussses"));
    assert!(syntax("device:\n  - name: A\n").contains("device"));
    assert!(!syntax("devices:\n  - name: A\n    id: a\n    busses: many\n").is_empty());
    // A wire end that is no channel or bus is reported as such.
    assert!(invalid("devices:\n  - name: A\n    id: a\n    inputs:\n      - virtual: true\n        wires: {up: 1}\n")[0].contains("'up' is not a channel"));
    assert!(!syntax("devices:\n  - name: A\n    id: a\n    inputs:\n      - virtual: true\n        wires: {-1: 1}\n").is_empty());
    // An input without wires is unpatched.
    assert!(parse("devices:\n  - name: A\n    id: a\n    inputs:\n      - virtual: true\n").unwrap().devices[0].inputs[0].wires.is_empty());
    // Duplicate keys are an error, not "last one wins".
    assert!(!syntax("devices:\n  - name: A\n    id: a\n    id: b\n").is_empty());
}

#[test]
fn earlier_key_names_say_what_they_are_now() {
    let p = invalid(concat!(
        "devices:\n",
        "  - name: D\n",
        "    id: d\n",
        "    channels: 4\n",
        "    pass_thru: 2\n",
        "    sources: [{pass_thru: true, wires: {1: 1}}]\n",
        "    monitors: [{output: X, wires: {1: 1}, volume: 100}]\n",
        "    inputs:\n",
        "      - {pass_thru: true, wires: {1: 1}}\n",
        "      - {input: Rane FOUR, wires: {1: 1}, volume: 0}\n",
        "    outputs:\n",
        "      - {output: PRO X, wires: {1: 1}}\n",
    ));
    let all = p.join("\n");
    for needle in [
        "'channels' is now 'busses'",
        "'pass_thru' is now 'virtual_inputs'",
        "'sources' is now 'inputs'",
        "'monitors' is now 'outputs'",
        "input 1: 'pass_thru: true' is now 'virtual: true'",
        "input 2: 'input:' is now 'device:'",
        "input 2: 'volume' is now 'gain'",
        "output 1 (device ''): 'output:' is now 'device:'",
    ] {
        assert!(all.contains(needle), "missing '{needle}' in:\n{all}");
    }
    assert_eq!(p.len(), 8, "only the renames, nothing else:\n{all}");
}

#[test]
fn device_problems_are_all_reported() {
    let p = invalid(concat!(
        "devices:\n",
        "  - {name: A, id: 'a#1', busses: 0, virtual_inputs: 65, sample_rate: 12345}\n",
        "  - {name: A, id: b}\n",
        "  - {name: '', id: b}\n",
    ));
    let all = p.join("\n");
    for needle in ["id 'a#1'", "busses must be 1-64", "virtual_inputs must be 0-64", "sample_rate", "same name", "device 3: name", "id 'b'"] {
        assert!(all.contains(needle), "missing '{needle}' in:\n{all}");
    }
    assert_eq!(p.len(), 7, "{all}");
}

#[test]
fn input_problems() {
    let dev = |inputs: &str| format!("devices:\n  - name: D\n    id: d\n    busses: 4\n    virtual_inputs: 2\n    inputs:\n{inputs}");
    let one = |inputs: &str| {
        let p = invalid(&dev(inputs));
        assert_eq!(p.len(), 1, "{p:?}");
        p[0].clone()
    };
    assert!(one("      - {wires: {1: 1}}\n").contains("needs one of"));
    assert!(one("      - {device: X, app: Y, wires: {1: 1}}\n").contains("more than one"));
    assert!(one("      - {virtual: false, wires: {1: 1}}\n").contains("enabled: false"));
    assert!(one("      - {virtual: true, wires: {3: 1}}\n").contains("input channel 3 does not exist"));
    assert!(one("      - {device: X, wires: {1: 5}}\n").contains("bus channel 5 does not exist"));
    assert!(one("      - {device: X, wires: {0: 1}}\n").contains("from 1"));
    assert!(one("      - {device: X, wires: {1: []}}\n").contains("empty list"));
    assert!(one("      - {device: X, wires: {1: [2, 2]}}\n").contains("listed twice"));
    assert!(one("      - {device: X, wires: {1: 1, left: 2}}\n").contains("channel 1 is listed twice"));
    assert!(one("      - {device: X, wires: {1: 1}, gain: 100}\n").contains("gain is in dB (0 = unity, -inf = silent) and at most +12, not 100"));
    assert!(invalid("devices:\n  - {name: R, id: r, buffer_frames: 8}\n")[0].contains("buffer_frames must be 16-4096"));
    assert!(one("      - {device: X, wires: {1: 1}, mute_original: true}\n").contains("app inputs"));
    assert!(one("      - {device: X, wires: {1: 1}, tap: true}\n").contains("tap only applies to app inputs"));
    let tapped = parse(&dev("      - {app: Spotify, wires: {1: 1}, tap: true}\n")).unwrap();
    assert_eq!(tapped.devices[0].inputs[0].kind, InputKind::App { name: "Spotify".into(), mute_original: false, tap: true });
    assert!(one("      - {device: ' ', wires: {1: 1}}\n").contains("name must not be empty"));
    assert!(one("      - {virtual: true, wires: {1: 1}}\n      - {virtual: true, wires: {2: 2}}\n").contains("only one 'virtual: true'"));

    let p = invalid("devices:\n  - {name: R, id: r, virtual_inputs: 0, inputs: [{virtual: true, wires: {1: 1}}]}\n");
    assert!(p.iter().any(|m| m.contains("virtual_inputs: 0")), "{p:?}");

    // Hardware and app inputs have run-time channel counts: high numbers are fine.
    parse(&dev("      - {device: Rane FOUR, wires: {12: 4}}\n")).unwrap();
}

#[test]
fn gains_in_db() {
    let db = |v: &str| parse(&format!("devices:\n  - name: D\n    id: d\n    outputs:\n      - {{device: X, wires: {{1: 1}}, gain: {v}}}\n")).map(|c| c.devices[0].outputs[0].gain_db);
    assert_eq!(db("0").unwrap(), 0.0);
    assert_eq!(db("-6").unwrap(), -6.0);
    assert_eq!(db("3.5").unwrap(), 3.5);
    assert_eq!(db("+12").unwrap(), 12.0);
    assert_eq!(db("'-6 dB'").unwrap(), -6.0);
    assert_eq!(db("-4.5dB").unwrap(), -4.5);
    assert_eq!(db("-inf").unwrap(), f64::NEG_INFINITY);
    assert_eq!(db("'-inf dB'").unwrap(), f64::NEG_INFINITY);
    assert!(db("12.1").is_err());
    assert!(db("loud").is_err());
    assert!(db(".inf").is_err());
    assert!(db(".nan").is_err());
}

#[test]
fn output_problems() {
    let p = invalid("devices:\n  - name: D\n    id: d\n    outputs:\n      - {device: X, wires: {3: 1}, gain: 200}\n      - {wires: {1: 1}}\n");
    let all = p.join("\n");
    assert!(all.contains("output 1 (device 'X'): wires: bus channel 3 does not exist (it has 2)"), "{all}");
    assert!(all.contains("output 1 (device 'X'): gain"), "{all}");
    assert!(all.contains("output 2 (device ''): needs 'device:' (a hardware output) or 'virtual: <name>'"), "{all}");
    parse("devices:\n  - name: D\n    id: d\n    outputs:\n      - {device: X, wires: {1: 13, 2: 14}, enabled: false, muted: true}\n").unwrap();
}

#[test]
fn virtual_devices() {
    let c = parse(concat!(
        "devices:\n",
        "  - name: Mixer\n",
        "    id: mixer\n",
        "    busses: 8\n",
        "    virtual_inputs: 0\n",
        "    inputs:\n",
        "      - {virtual: Comms, channels: 2, wires: {1: 1, 2: 2}}\n",
        "      - {virtual: Daw, channels: 16, id: fl, wires: {1: 3, 16: 4}}\n",
        "    outputs:\n",
        "      - {virtual: Daw Output, channels: 4, wires: {1: 1, 2: 2, 5: 3}}\n",
        "      - {device: PRO X, wires: {1: 1}}\n",
    ))
    .unwrap();
    let d = &c.devices[0];
    assert_eq!(d.inputs[0].kind, InputKind::VirtualDevice(VirtualDevice { name: "Comms".into(), id: "comms".into(), channels: 2, icon: None }));
    assert_eq!(d.inputs[1].kind, InputKind::VirtualDevice(VirtualDevice { name: "Daw".into(), id: "fl".into(), channels: 16, icon: None }));
    assert_eq!(d.outputs[0].kind, OutputKind::Virtual(VirtualDevice { name: "Daw Output".into(), id: "daw-output".into(), channels: 4, icon: None }));
    assert_eq!(d.outputs[0].wires, vec![w(1, 1), w(2, 2), w(5, 3)]);
    let v = &c.to_specs()[0].virtuals;
    let ids: Vec<(&str, bool)> = v.iter().map(|v| (v.id.as_str(), v.output)).collect();
    assert_eq!(ids, [("comms", false), ("fl", false), ("daw-output", true)], "inputs first, then outputs, in config order");
    assert_eq!(lc_spec::parse(&lc_spec::to_json(&c.to_specs())).unwrap(), c.to_specs(), "the plug-in accepts it");
}

#[test]
fn virtual_device_problems() {
    let p = |entries: &str| invalid(&format!("devices:\n  - name: M\n    id: m\n    busses: 4\n{entries}"));
    let one = |entries: &str| {
        let v = p(entries);
        assert_eq!(v.len(), 1, "{v:?}");
        v[0].clone()
    };
    assert!(one("    inputs:\n      - {virtual: Comms, wires: {1: 1}}\n").contains("needs 'channels:'"));
    assert!(one("    inputs:\n      - {virtual: Comms, channels: 2, wires: {3: 1}}\n").contains("input channel 3 does not exist (it has 2)"));
    assert!(one("    outputs:\n      - {virtual: Rec, channels: 2, wires: {1: 3}}\n").contains("device channel 3 does not exist (it has 2)"));
    assert!(one("    inputs:\n      - {virtual: true, channels: 2, wires: {1: 1}}\n").contains("only apply to a virtual device"));
    assert!(one("    outputs:\n      - {virtual: true, wires: {1: 1}}\n").contains("not true/false"));
    assert!(one("    outputs:\n      - {virtual: X, device: Y, channels: 2, wires: {1: 1}}\n").contains("both"));
    assert!(one("    inputs:\n      - {virtual: '!!!', channels: 2, wires: {1: 1}}\n").contains("set one with 'id:'"));
    let same = one("    inputs:\n      - {virtual: Daw, channels: 2, wires: {1: 1}}\n    outputs:\n      - {virtual: daw, channels: 2, wires: {1: 1}}\n");
    assert!(same.contains("has the id 'daw'"), "{same}");
}

#[test]
fn outputs_can_follow_the_device_volume() {
    let c = parse("devices:\n  - name: D\n    id: d\n    outputs:\n      - {device: Headset, wires: {1: 1}, follow_volume: true}\n      - {device: Mixer, wires: {1: 9}}\n").unwrap();
    let d = &c.devices[0];
    assert!(d.outputs[0].follow_volume && !d.outputs[1].follow_volume);
    assert!(d.volume_follows_outputs());
    assert!(!c.to_specs()[0].apply_gain, "the plug-in then leaves the volume to the daemon");
    assert!(parse("devices:\n  - {name: D, id: d}\n").unwrap().to_specs()[0].apply_gain);
}

#[test]
fn error_display_lists_problems() {
    let e = parse("devices:\n  - {name: A, id: a, busses: 0}\n").unwrap_err().to_string();
    assert_eq!(e, "invalid configuration:\n  - device 'A': busses must be 1-64, not 0");
}

#[test]
fn load_reports_missing_file() {
    let e = load(Path::new("/nonexistent/loopcraft.yaml")).unwrap_err();
    assert!(matches!(e, Error::Read { .. }));
    assert!(e.to_string().contains("/nonexistent/loopcraft.yaml"));
}

#[test]
fn finds_devices() {
    let hw = |uid: &str, name: &str| HardwareDevice { uid: uid.into(), name: name.into() };
    let devs = vec![
        hw("AppleUSBAudioEngine:Rane:Rane FOUR:A:3,4", "Rane FOUR"),
        hw("AppleUSBAudioEngine:Logitech:PRO X:1", "PRO X Wireless Gaming Headset"),
        hw("usb-1", "USB Audio"),
        hw("usb-2", "USB Audio"),
        hw("BuiltInSpeakerDevice", "MacBook Pro Speakers"),
    ];
    assert_eq!(find_device("Rane FOUR", &devs).unwrap().uid, devs[0].uid);
    assert_eq!(find_device("usb-2", &devs).unwrap().uid, "usb-2");
    assert_eq!(find_device("pro x", &devs).unwrap().name, devs[1].name);
    assert_eq!(find_device("speakers", &devs).unwrap().uid, "BuiltInSpeakerDevice");
    assert_eq!(find_device("USB Audio", &devs), Err(MatchError::Ambiguous(vec!["USB Audio".into(), "USB Audio".into()])));
    assert!(matches!(find_device("a", &devs), Err(MatchError::Ambiguous(_))));
    assert_eq!(find_device("Focusrite", &devs), Err(MatchError::NotFound));
    assert_eq!(find_device(" ", &devs), Err(MatchError::NotFound));
}

#[test]
fn named_busses() {
    let c = parse("devices:\n  - name: A\n    id: a\n    busses: [Program, Music, DJ]\n    inputs: [{virtual: true, wires: {1: 5, 2: 6}}]\n").unwrap();
    let d = &c.devices[0];
    assert_eq!(d.busses, 6, "one stereo bus per name");
    assert_eq!(d.bus_list.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["Program", "Music", "DJ"]);
    let numbered = parse("devices:\n  - {name: A, id: a, busses: 10}\n").unwrap();
    assert_eq!(numbered.devices[0].bus_list.len(), 5);
    assert!(numbered.devices[0].bus_list.iter().all(|b| b.name.is_empty() && b.id.starts_with('b')));
    // Wires reach only the named busses' channels.
    assert!(invalid("devices:\n  - {name: A, id: a, busses: [Program], inputs: [{virtual: true, wires: {1: 3}}]}\n").iter().any(|p| p.contains("bus")));
    let problems = invalid("devices:\n  - {name: A, id: a, busses: [Main, \" \", main]}\n");
    assert!(problems.contains(&"device 'A': bus 3/4 needs a name".to_string()), "{problems:?}");
    assert!(problems.contains(&"device 'A': two busses are called 'main'".to_string()), "{problems:?}");
    assert!(invalid("devices:\n  - {name: A, id: a, busses: []}\n").iter().any(|p| p.contains("1-64 channels in all")));
}

#[test]
fn mono_busses_and_groups() {
    let text = concat!(
        "devices:\n  - name: M\n    id: m\n",
        "    groups: [{id: g00000001, name: Comms}]\n",
        "    busses:\n",
        "      - {id: b00000001, name: Program}\n",
        "      - {id: b00000002, name: Mic, mono: true, group: Comms}\n",
        "      - {id: b00000003, mono: true, group: g00000001}\n",
        "      - {id: b00000004, name: Music}\n",
        "    inputs:\n",
        "      - {device: PRO X, to: Mic}\n",
        "      - {device: X18, wires: {1: b00000003, 2: [Mic, Music.left]}, mono: [2, 1, 2], half: true}\n",
        "      - {app: Spotify, to: Mic}\n",
        "    outputs:\n",
        "      - {device: Headset, from: Mic}\n",
    );
    let c = parse(text).unwrap();
    let d = &c.devices[0];
    assert_eq!(d.busses, 6, "two stereo and two mono busses");
    assert_eq!(d.bus_channels(1), (3, None));
    assert_eq!(d.bus_channels(3), (5, Some(6)));
    assert_eq!(d.bus_of(4), Some(2));
    assert_eq!(d.bus_of(6), Some(3));
    assert_eq!(d.bus_of(7), None);
    assert_eq!(d.bus_list[1].group.as_deref(), Some("g00000001"), "a group by name is kept by its id");
    assert_eq!(d.bus_list[2].group.as_deref(), Some("g00000001"));
    let w = |i: &crate::Input| i.wires.iter().map(|w| (w.from, w.to)).collect::<Vec<_>>();
    assert_eq!(w(&d.inputs[0]), [(1, 3), (2, 3)], "both sides of a stereo unit into a mono bus");
    assert_eq!(w(&d.inputs[1]), [(1, 4), (2, 3), (2, 5)]);
    assert_eq!(d.inputs[1].mono, [1, 2]);
    assert!(d.inputs[1].half);
    assert_eq!(d.outputs[0].wires.iter().map(|w| (w.from, w.to)).collect::<Vec<_>>(), [(3, 1), (3, 2)], "a mono bus to both sides");
    let yaml = crate::to_yaml(&c);
    assert!(yaml.contains("{id: \"b00000002\", name: \"Mic\", mono: true, group: \"g00000001\"}"), "{yaml}");
    assert!(yaml.contains("wires: {1: \"b00000003\", 2: [\"b00000002\", \"b00000004.left\"]}"), "{yaml}");
    assert!(yaml.contains("        to: \"b00000002\""), "{yaml}");
    assert_eq!(parse(&yaml).unwrap(), c, "round trip of:\n{yaml}");
    let problems = invalid(concat!(
        "devices:\n  - name: M\n    id: m\n",
        "    busses: [{id: b1, mono: true, group: Nope}, {id: b2}]\n",
        "    inputs: [{device: X, wires: {1: b2}}, {virtual: Comms, channels: 2, mono: [3]}]\n",
    ));
    assert!(problems.iter().any(|p| p.contains("bus 1: there is no bus group 'Nope'")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("say which side of the bus")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("mono: channel 3 does not exist")), "{problems:?}");
}

/// Wires that name busses by id or name, with the stereo shorthands.
const BY_ID: &str = concat!(
    "devices:\n  - name: M\n    id: m\n    virtual_inputs: 2\n",
    "    busses:\n",
    "      - {id: b00000001, name: Program}\n",
    "      - {id: b00000002, name: Music}\n",
    "      - {id: b00000003}\n",
    "    inputs:\n",
    "      - {virtual: true, id: i00000001, to: b00000001}\n",
    "      - {app: Spotify, id: i00000002, to: [music, b00000003]}\n",
    "      - {device: PRO X, id: i00000003, wires: {1: [b00000003.left, b00000003.right]}}\n",
    "      - {device: Rane FOUR, wires: {5: Music.left, 6: music.R}}\n",
    "    outputs:\n",
    "      - {device: Headset, id: o00000001, from: [b00000001, Music]}\n",
    "      - {virtual: Daw Send, channels: 4, wires: {b00000002.left: 1, b00000002.right: 2, b00000003.left: 3}}\n",
);

#[test]
fn wires_name_busses_by_id_or_name() {
    let c = parse(BY_ID).unwrap();
    let d = &c.devices[0];
    assert_eq!(d.busses, 6);
    let ws = |v: &[Wire]| v.iter().map(|w| (w.from, w.to)).collect::<Vec<_>>();
    assert_eq!(ws(&d.inputs[0].wires), [(1, 1), (2, 2)], "to: stereo");
    assert_eq!(ws(&d.inputs[1].wires), [(1, 3), (1, 5), (2, 4), (2, 6)], "by name (any case) and id");
    assert_eq!(ws(&d.inputs[2].wires), [(1, 5), (1, 6)], "a mono mic onto both sides");
    assert_eq!(ws(&d.inputs[3].wires), [(5, 3), (6, 4)]);
    assert_eq!(ws(&d.outputs[0].wires), [(1, 1), (2, 2), (3, 1), (4, 2)], "from: stereo");
    assert_eq!(ws(&d.outputs[1].wires), [(3, 1), (4, 2), (5, 3)]);
    assert_eq!(d.inputs[1].id, "i00000002");
    assert!(d.inputs[3].id.starts_with('i'), "an id is made for a unit without one");
    assert_eq!(d.outputs[1].id, "daw-send", "a virtual device's id is its device id");
    // Read twice, the same ids.
    assert_eq!(parse(BY_ID).unwrap(), c);
}

#[test]
fn bad_bus_references() {
    let base = "devices:\n  - name: M\n    id: m\n    busses: [{id: b1, name: Music}]\n    inputs:\n";
    let p = invalid(&format!("{base}      - {{app: A, to: Beats}}\n"));
    assert!(p.iter().any(|p| p.contains("to: there is no bus 'Beats'")), "{p:?}");
    let p = invalid(&format!("{base}      - {{app: A, wires: {{left: Music}}}}\n"));
    assert!(p.iter().any(|p| p.contains("say which side of the bus")), "{p:?}");
    let p = invalid(&format!("{base}      - {{app: A, wires: {{left: Music.middle}}}}\n"));
    assert!(p.iter().any(|p| p.contains("not .middle")), "{p:?}");
    let p = invalid(&format!("{base}      - {{app: A, id: b1, to: Music}}\n"));
    assert!(p.iter().any(|p| p.contains("another bus, input or output of this mixer has the id 'b1'")), "{p:?}");
}

#[test]
fn renaming_or_reordering_busses_keeps_the_patch() {
    let c = parse(BY_ID).unwrap();
    // Swap Program and Music in the file: wires follow the busses, not the places.
    let swapped = BY_ID.replace("      - {id: b00000001, name: Program}\n      - {id: b00000002, name: Music}\n", "      - {id: b00000002, name: Music}\n      - {id: b00000001, name: Program}\n");
    let s = parse(&swapped).unwrap();
    let d = &s.devices[0];
    assert_eq!(d.inputs[0].wires.iter().map(|w| w.to).collect::<Vec<_>>(), [3, 4], "System follows Program to 3/4");
    assert_ne!(c, s);
    // And the editor's save writes it back with the same ids.
    let back = parse(&to_yaml(&s)).unwrap();
    assert_eq!(back, s);
}
