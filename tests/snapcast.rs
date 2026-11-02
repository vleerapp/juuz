mod common;

use std::io::Read;
use std::net::TcpStream;
use std::time::Duration;

use common::*;

async fn song(h: &Harness) -> String {
    make_track(
        &h.root(),
        "Band/2020 - Album/01 - One.wav",
        3,
        &Tags {
            title: Some("One"),
            artist: Some("Band"),
            album: Some("Album"),
            track: Some(1),
            ..Tags::default()
        },
    );
    h.scan().await;
    let v = h.ok("getRandomSongs", "size=1").await;
    v["randomSongs"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn speakers_get_the_codec_header_and_timed_audio() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let id = song(&h).await;
    let port = h.server.state.jukebox.snapcast_port().expect("listening");

    let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    hello(&mut sock, "aa:bb", "test speaker");

    let settings = receive(&mut sock);
    assert_eq!(settings.kind, 3);
    assert_eq!(settings.refers_to, 1, "the hello must be answered");
    let json: serde_json::Value = serde_json::from_slice(&settings.body[4..]).unwrap();
    assert_eq!(json["bufferMs"], 1000);
    assert_eq!(json["volume"], 50, "speakers start at half volume");

    let codec = receive(&mut sock);
    assert_eq!(codec.kind, 1);
    assert_eq!(&codec.body[4..7], b"pcm");
    assert_eq!(&codec.body[11..15], b"RIFF");

    let listed = loop {
        let v = h.ok("juuzGetJukeboxOutputs", "").await;
        if !v["juuzJukeboxOutputs"]["device"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            break v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let device = &listed["juuzJukeboxOutputs"]["device"][0];
    assert_eq!(device["id"], "snapcast:aa:bb");
    assert_eq!(device["name"], "test speaker");
    let v = h.ok("juuzSetJukeboxOutput", "id=snapcast:aa:bb").await;
    assert_eq!(v["juuzJukeboxOutputs"]["current"]["id"], "snapcast:aa:bb");

    send(&mut sock, 4, 9, &[0; 8]);
    let time = loop {
        let m = receive(&mut sock);
        if m.kind == 4 {
            break m;
        }
    };
    assert_eq!(time.body.len(), 8);

    h.ok("jukeboxControl", &format!("action=set&id={id}")).await;
    h.ok("jukeboxControl", "action=start").await;

    let mut stamps = Vec::new();
    let mut loud = false;
    while stamps.len() < 6 {
        let m = receive(&mut sock);
        if m.kind != 2 {
            continue;
        }
        let sec = i32::from_le_bytes(m.body[0..4].try_into().unwrap());
        let usec = i32::from_le_bytes(m.body[4..8].try_into().unwrap());
        let size = u32::from_le_bytes(m.body[8..12].try_into().unwrap()) as usize;
        assert_eq!(size, 960 * 4);
        assert_eq!(m.body.len(), 12 + size);
        loud |= m.body[12..]
            .as_chunks::<2>()
            .0
            .iter()
            .any(|s| i16::from_le_bytes(*s).unsigned_abs() > 1000);
        stamps.push(i64::from(sec) * 1_000_000 + i64::from(usec));
    }
    assert!(loud, "the tone should come through");
    for pair in stamps.windows(2) {
        assert_eq!(pair[1] - pair[0], 20_000, "{stamps:?}");
    }

    let outputs = h.ok("juuzGetJukeboxOutputs", "").await;
    assert_eq!(outputs["juuzJukeboxOutputs"]["speakers"][0], "test speaker");

    h.ok("jukeboxControl", "action=setGain&gain=0.5").await;
    loop {
        let m = receive(&mut sock);
        if m.kind == 3 {
            let json: serde_json::Value = serde_json::from_slice(&m.body[4..]).unwrap();
            assert_eq!(json["volume"], 50);
            break;
        }
    }

    h.ok("jukeboxControl", "action=stop").await;
    h.ok("juuzSetJukeboxOutput", "").await;
}

async fn wait_for_devices(h: &Harness, count: usize) -> serde_json::Value {
    for _ in 0..100 {
        let v = h.ok("juuzGetJukeboxOutputs", "").await;
        if v["juuzJukeboxOutputs"]["device"].as_array().unwrap().len() == count {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never saw {count} devices");
}

fn latency_of(sock: &mut TcpStream, want: i64) -> i64 {
    let mut seen = Vec::new();
    for _ in 0..400 {
        let m = receive(sock);
        if m.kind != 3 {
            continue;
        }
        let json: serde_json::Value = serde_json::from_slice(&m.body[4..]).unwrap();
        let latency = json["latency"].as_i64().unwrap();
        if latency == want {
            return latency;
        }
        seen.push(latency);
    }
    panic!("never saw latency {want}, saw {seen:?}");
}

fn next_chunk(sock: &mut TcpStream) -> Message {
    loop {
        let m = receive(sock);
        if m.kind == 2 {
            return m;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_speaker_is_its_own_output_with_its_own_volume() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let id = song(&h).await;
    let mut kitchen = speaker(&h, "aa:01", "kitchen");
    let mut study = speaker(&h, "aa:02", "study");

    let v = wait_for_devices(&h, 2).await;
    let ids: Vec<&str> = v["juuzJukeboxOutputs"]["device"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"snapcast:aa:01") && ids.contains(&"snapcast:aa:02"));

    h.ok("juuzSetJukeboxOutput", "id=snapcast:aa:02").await;
    h.ok("jukeboxControl", &format!("action=set&id={id}")).await;
    h.ok("jukeboxControl", "action=start").await;
    next_chunk(&mut study);

    h.ok("jukeboxControl", "action=setGain&gain=0.3").await;
    loop {
        let m = receive(&mut study);
        if m.kind == 3 {
            let json: serde_json::Value = serde_json::from_slice(&m.body[4..]).unwrap();
            assert_eq!(json["volume"], 30);
            break;
        }
    }

    kitchen
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut heard = false;
    let mut head = [0u8; 26];
    if kitchen.read_exact(&mut head).is_ok() {
        heard = u16::from_le_bytes([head[0], head[1]]) == 2;
    }
    assert!(!heard, "the other speaker must stay silent");
    h.ok("jukeboxControl", "action=stop").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_feeds_every_speaker_with_its_own_delay() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let id = song(&h).await;
    let mut kitchen = speaker(&h, "aa:01", "kitchen");
    let mut study = speaker(&h, "aa:02", "study");
    wait_for_devices(&h, 2).await;

    let v = h
        .ok(
            "juuzSetJukeboxGroup",
            "name=Downstairs&member=snapcast:aa:01&member=snapcast:aa:02&delay=0&delay=250",
        )
        .await;
    let group = &v["juuzJukeboxOutputs"]["group"][0];
    assert_eq!(group["name"], "Downstairs");
    assert_eq!(group["member"][1]["delayMs"], 250);
    let gid = group["id"].as_str().unwrap().to_string();
    assert!(gid.starts_with("group:"));

    let v = h.ok("juuzSetJukeboxOutput", &format!("id={gid}")).await;
    assert_eq!(v["juuzJukeboxOutputs"]["current"]["protocol"], "group");
    h.ok("jukeboxControl", &format!("action=set&id={id}")).await;
    h.ok("jukeboxControl", "action=start").await;

    assert_eq!(latency_of(&mut kitchen, 0), 0);
    assert_eq!(
        latency_of(&mut study, -250),
        -250,
        "a later speaker is a negative client latency"
    );
    next_chunk(&mut kitchen);
    next_chunk(&mut study);

    h.ok("jukeboxControl", "action=setGain&gain=0.3").await;
    for sock in [&mut kitchen, &mut study] {
        loop {
            let m = receive(sock);
            if m.kind == 3 {
                let json: serde_json::Value = serde_json::from_slice(&m.body[4..]).unwrap();
                if json["volume"] == 30 {
                    break;
                }
            }
        }
    }

    h.ok("jukeboxControl", "action=stop").await;
    let v = h.ok("juuzDeleteJukeboxGroup", &format!("id={gid}")).await;
    assert!(
        v["juuzJukeboxOutputs"]["group"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(v["juuzJukeboxOutputs"].get("current").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn groups_reject_bad_members() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let _kitchen = speaker(&h, "aa:01", "kitchen");
    wait_for_devices(&h, 1).await;
    for query in [
        "name=&member=snapcast:aa:01",
        "name=x",
        "name=x&member=snapcast:nope",
        "name=x&member=snapcast:aa:01&member=snapcast:aa:01",
        "name=x&member=snapcast:aa:01&delay=soon",
    ] {
        let v = h.call("juuzSetJukeboxGroup", query).await;
        assert_eq!(v["status"], "failed", "{query}: {v}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_a_delay_retimes_the_running_group_without_reopening_it() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let id = song(&h).await;
    let mut kitchen = speaker(&h, "aa:01", "kitchen");
    let mut study = speaker(&h, "aa:02", "study");
    wait_for_devices(&h, 2).await;

    let v = h
        .ok(
            "juuzSetJukeboxGroup",
            "name=Downstairs&member=snapcast:aa:01&member=snapcast:aa:02&delay=0&delay=0",
        )
        .await;
    let gid = v["juuzJukeboxOutputs"]["group"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok("juuzSetJukeboxOutput", &format!("id={gid}")).await;
    h.ok("jukeboxControl", &format!("action=set&id={id}")).await;
    h.ok("jukeboxControl", "action=start").await;

    next_chunk(&mut kitchen);
    next_chunk(&mut study);

    let v = h
        .ok(
            "juuzSetJukeboxGroup",
            &format!(
                "id={gid}&name=Downstairs&member=snapcast:aa:01&member=snapcast:aa:02&delay=0&delay=400"
            ),
        )
        .await;
    assert_eq!(v["juuzJukeboxOutputs"]["current"]["id"], gid.as_str());
    assert_eq!(
        v["juuzJukeboxOutputs"]["group"][0]["member"][1]["delayMs"],
        400
    );

    latency_of(&mut study, -400);
    latency_of(&mut kitchen, 0);
    h.ok("jukeboxControl", "action=stop").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_can_join_a_group_next_to_a_speaker() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let id = song(&h).await;
    let mut kitchen = speaker(&h, "aa:01", "kitchen");
    wait_for_devices(&h, 1).await;

    for bad in ["browser:", "browser:Upper", "browser:a%20b", "browser:a:b"] {
        let v = h
            .call(
                "juuzSetJukeboxGroup",
                &format!("name=x&member=snapcast:aa:01&member={bad}"),
            )
            .await;
        assert_eq!(v["status"], "failed", "{bad}: {v}");
    }

    let v = h
        .ok(
            "juuzSetJukeboxGroup",
            "name=Mixed&member=snapcast:aa:01&member=browser:firefox-a1b2&delay=0&delay=120",
        )
        .await;
    let group = &v["juuzJukeboxOutputs"]["group"][0];
    assert_eq!(group["member"][1]["delayMs"], 120);
    let gid = group["id"].as_str().unwrap().to_string();

    h.ok("juuzSetJukeboxOutput", &format!("id={gid}")).await;
    h.ok("jukeboxControl", &format!("action=set&id={id}")).await;
    h.ok("jukeboxControl", "action=start").await;
    next_chunk(&mut kitchen);

    h.ok(
        "juuzSetJukeboxGroup",
        &format!("id={gid}&name=Mixed&member=snapcast:aa:01&delay=0"),
    )
    .await;
    next_chunk(&mut kitchen);
    h.ok("jukeboxControl", "action=stop").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_and_removing_members_keeps_the_others_playing() {
    let h = Harness::build(None, |c| c.snapcast_port = 0);
    let id = song(&h).await;
    let mut kitchen = speaker(&h, "aa:01", "kitchen");
    let mut study = speaker(&h, "aa:02", "study");
    wait_for_devices(&h, 2).await;

    let v = h
        .ok(
            "juuzSetJukeboxGroup",
            "name=Downstairs&member=snapcast:aa:01&delay=0",
        )
        .await;
    let gid = v["juuzJukeboxOutputs"]["group"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok("juuzSetJukeboxOutput", &format!("id={gid}")).await;
    h.ok("jukeboxControl", &format!("action=set&id={id}")).await;
    h.ok("jukeboxControl", "action=start").await;
    next_chunk(&mut kitchen);

    h.ok(
        "juuzSetJukeboxGroup",
        &format!(
            "id={gid}&name=Downstairs&member=snapcast:aa:01&member=snapcast:aa:02&delay=0&delay=0"
        ),
    )
    .await;
    next_chunk(&mut study);
    next_chunk(&mut kitchen);

    h.ok(
        "juuzSetJukeboxGroup",
        &format!("id={gid}&name=Downstairs&member=snapcast:aa:02&delay=0"),
    )
    .await;
    next_chunk(&mut study);
    let v = h.ok("jukeboxControl", "action=status").await;
    assert_eq!(v["jukeboxStatus"]["playing"], true);
    h.ok("jukeboxControl", "action=stop").await;
}
