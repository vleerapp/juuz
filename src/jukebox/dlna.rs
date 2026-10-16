use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use roxmltree::Document;
use socket2::{Domain, Protocol, Socket, Type};

const SSDP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
const AV_TRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RENDERING_CONTROL: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
const MEDIA_RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const DISCOVERY_WINDOW: Duration = Duration::from_millis(2500);
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub udn: String,
    pub name: String,
    pub location: String,
}

#[derive(Clone)]
pub struct Renderer {
    pub device: Device,
    av_url: String,
    rc_url: Option<String>,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Default)]
pub struct Position {
    pub track_uri: Option<String>,
    pub seconds: Option<u64>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && !matches!(c, '\n' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

fn origin(url: &str) -> Result<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow!("url must be http(s)"))?;
    let scheme_len = url.len() - rest.len();
    let end = rest.find('/').unwrap_or(rest.len());
    Ok(&url[..scheme_len + end])
}

fn resolve(base: &str, target: &str) -> Result<String> {
    let target = target.trim();
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(target.to_string());
    }
    if target.starts_with('/') {
        return Ok(format!("{}{target}", origin(base)?));
    }
    let dir = match base.rfind('/') {
        Some(i) if i >= origin(base)?.len() => &base[..=i],
        _ => &format!("{}/", origin(base)?),
    };
    Ok(format!("{dir}{target}"))
}

fn host_port(url: &str) -> Result<SocketAddr> {
    let o = origin(url)?;
    let https = o.starts_with("https://");
    let authority = o.split("://").nth(1).unwrap_or_default();
    let target = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:{}", if https { 443 } else { 80 })
    };
    target
        .to_socket_addrs()?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| anyhow!("could not resolve {authority}"))
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

struct Described {
    device: Device,
    av_url: String,
    rc_url: Option<String>,
}

fn describe(agent: &ureq::Agent, location: &str) -> Result<Described> {
    origin(location)?;
    let mut resp = agent
        .get(location)
        .call()
        .with_context(|| format!("fetching {location}"))?;
    if !resp.status().is_success() {
        bail!("{location} returned {}", resp.status());
    }
    let xml = resp.body_mut().read_to_string()?;
    let doc = Document::parse(&xml).context("parsing device description")?;
    let base = doc
        .descendants()
        .find(|n| n.tag_name().name() == "URLBase")
        .and_then(|n| n.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(location)
        .to_string();

    for node in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "device")
    {
        let mut av = None;
        let mut rc = None;
        if let Some(list) = node
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == "serviceList")
        {
            for service in list.children().filter(|c| c.is_element()) {
                let (Some(kind), Some(control)) = (
                    child_text(service, "serviceType"),
                    child_text(service, "controlURL"),
                ) else {
                    continue;
                };
                if kind.starts_with("urn:schemas-upnp-org:service:AVTransport:") {
                    av = Some(resolve(&base, control)?);
                } else if kind.starts_with("urn:schemas-upnp-org:service:RenderingControl:") {
                    rc = Some(resolve(&base, control)?);
                }
            }
        }
        let (Some(av_url), Some(udn)) = (av, child_text(node, "UDN")) else {
            continue;
        };
        let name = child_text(node, "friendlyName").unwrap_or(udn).to_string();
        return Ok(Described {
            device: Device {
                udn: udn.to_string(),
                name,
                location: location.to_string(),
            },
            av_url,
            rc_url: rc,
        });
    }
    bail!("{location} is not a media renderer")
}

pub fn describe_device(location: &str) -> Result<Device> {
    Ok(describe(&agent(), location)?.device)
}

fn search_request() -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {MEDIA_RENDERER}\r\nUSER-AGENT: juuz/1 UPnP/1.1\r\n\r\n"
    )
}

fn header(response: &str, name: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

fn sockets() -> Vec<UdpSocket> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for iface in ifaces {
        let IpAddr::V4(ip) = iface.ip() else {
            continue;
        };
        if ip.is_loopback() {
            continue;
        }
        let open = || -> std::io::Result<UdpSocket> {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            socket.bind(&SocketAddr::new(IpAddr::V4(ip), 0).into())?;
            socket.set_multicast_if_v4(&ip)?;
            socket.set_multicast_ttl_v4(2)?;
            socket.set_nonblocking(true)?;
            Ok(socket.into())
        };
        match open() {
            Ok(s) => out.push(s),
            Err(e) => tracing::debug!("ssdp socket on {ip} failed: {e}"),
        }
    }
    out
}

pub fn discover() -> Vec<Device> {
    let sockets = sockets();
    let request = search_request();
    for s in &sockets {
        for _ in 0..2 {
            let _ = s.send_to(request.as_bytes(), SSDP);
        }
    }

    let mut locations: Vec<String> = Vec::new();
    let deadline = Instant::now() + DISCOVERY_WINDOW;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        for s in &sockets {
            while let Ok((n, _)) = s.recv_from(&mut buf) {
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(loc) = header(&text, "location")
                    && !locations.contains(&loc)
                {
                    locations.push(loc);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let agent = agent();
    let mut found: Vec<Device> = Vec::new();
    for loc in locations {
        match describe(&agent, &loc) {
            Ok(d) if !found.iter().any(|f| f.udn == d.device.udn) => found.push(d.device),
            Ok(_) => {}
            Err(e) => tracing::debug!("skipping {loc}: {e:#}"),
        }
    }
    found
}

pub struct TrackMeta<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub content_type: &'a str,
    pub url: &'a str,
    pub duration_secs: u64,
}

pub fn didl(m: &TrackMeta) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">\
         <item id=\"0\" parentID=\"-1\" restricted=\"1\">\
         <dc:title>{}</dc:title><dc:creator>{}</dc:creator><upnp:artist>{}</upnp:artist><upnp:album>{}</upnp:album>\
         <upnp:class>object.item.audioItem.musicTrack</upnp:class>\
         <res protocolInfo=\"http-get:*:{}:*\" duration=\"{}\">{}</res>\
         </item></DIDL-Lite>",
        escape(m.title),
        escape(m.artist),
        escape(m.artist),
        escape(m.album),
        escape(m.content_type),
        hms(m.duration_secs),
        escape(m.url),
    )
}

pub fn hms(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

pub fn parse_hms(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("NOT_IMPLEMENTED") {
        return None;
    }
    let mut parts = text.split(':');
    let h: u64 = parts.next()?.trim_start_matches('+').parse().ok()?;
    let m: u64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600 + m * 60 + s as u64)
}

fn field(xml: &str, name: &str) -> Option<String> {
    let doc = Document::parse(xml).ok()?;
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .map(|n| n.text().unwrap_or_default().to_string())
}

fn soap(
    agent: &ureq::Agent,
    url: &str,
    service: &str,
    action: &str,
    args: &[(&str, &str)],
) -> Result<String> {
    let mut body = String::new();
    for (k, v) in args {
        body.push_str(&format!("<{k}>{}</{k}>", escape(v)));
    }
    let envelope = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><u:{action} xmlns:u=\"{service}\">{body}</u:{action}></s:Body></s:Envelope>"
    );
    tracing::debug!(
        "upnp {action} {}",
        args.iter()
            .map(|(k, v)| format!("{k}={}", v.chars().take(80).collect::<String>()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut resp = agent
        .post(url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", &format!("\"{service}#{action}\""))
        .send(envelope)
        .with_context(|| format!("{action} request failed"))?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if !status.is_success() {
        let code = field(&text, "errorCode").unwrap_or_default();
        let desc = field(&text, "errorDescription").unwrap_or_default();
        bail!("{action} failed: http {status} upnp {code} {desc}");
    }
    Ok(text)
}

impl Renderer {
    pub fn connect(location: &str) -> Result<Renderer> {
        let agent = agent();
        let d = describe(&agent, location)?;
        Ok(Renderer {
            device: d.device,
            av_url: d.av_url,
            rc_url: d.rc_url,
            agent,
        })
    }

    fn av(&self, action: &str, args: &[(&str, &str)]) -> Result<String> {
        soap(&self.agent, &self.av_url, AV_TRANSPORT, action, args)
    }

    fn rc(&self, action: &str, args: &[(&str, &str)]) -> Result<String> {
        let url = self
            .rc_url
            .as_deref()
            .ok_or_else(|| anyhow!("renderer has no volume control"))?;
        soap(&self.agent, url, RENDERING_CONTROL, action, args)
    }

    pub fn local_ip(&self) -> Result<IpAddr> {
        let target = host_port(&self.av_url)?;
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(target)?;
        Ok(socket.local_addr()?.ip())
    }

    pub fn set_uri(&self, uri: &str, metadata: &str) -> Result<()> {
        self.av(
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", uri),
                ("CurrentURIMetaData", metadata),
            ],
        )?;
        Ok(())
    }

    pub fn play(&self) -> Result<()> {
        self.av("Play", &[("InstanceID", "0"), ("Speed", "1")])?;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.av("Pause", &[("InstanceID", "0")])?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.av("Stop", &[("InstanceID", "0")])?;
        Ok(())
    }

    pub fn seek(&self, secs: u64) -> Result<()> {
        self.av(
            "Seek",
            &[
                ("InstanceID", "0"),
                ("Unit", "REL_TIME"),
                ("Target", &hms(secs)),
            ],
        )?;
        Ok(())
    }

    pub fn transport_state(&self) -> Result<String> {
        let xml = self.av("GetTransportInfo", &[("InstanceID", "0")])?;
        field(&xml, "CurrentTransportState").ok_or_else(|| anyhow!("no transport state"))
    }

    pub fn position(&self) -> Result<Position> {
        let xml = self.av("GetPositionInfo", &[("InstanceID", "0")])?;
        Ok(Position {
            track_uri: field(&xml, "TrackURI").filter(|u| !u.is_empty()),
            seconds: field(&xml, "RelTime").and_then(|t| parse_hms(&t)),
        })
    }

    pub fn volume(&self) -> Result<u32> {
        let xml = self.rc("GetVolume", &[("InstanceID", "0"), ("Channel", "Master")])?;
        field(&xml, "CurrentVolume")
            .and_then(|v| v.trim().parse().ok())
            .ok_or_else(|| anyhow!("no volume"))
    }

    pub fn set_volume(&self, percent: u32) -> Result<()> {
        self.rc(
            "SetVolume",
            &[
                ("InstanceID", "0"),
                ("Channel", "Master"),
                ("DesiredVolume", &percent.min(100).to_string()),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_control_urls() {
        let base = "http://10.0.0.5:49152/desc/root.xml";
        assert_eq!(
            resolve(base, "/ctl/av").unwrap(),
            "http://10.0.0.5:49152/ctl/av"
        );
        assert_eq!(
            resolve(base, "ctl/av").unwrap(),
            "http://10.0.0.5:49152/desc/ctl/av"
        );
        assert_eq!(resolve(base, "http://x/y").unwrap(), "http://x/y");
        assert_eq!(
            resolve("http://10.0.0.5:49152", "ctl").unwrap(),
            "http://10.0.0.5:49152/ctl"
        );
    }

    #[test]
    fn time_roundtrip() {
        assert_eq!(hms(3725), "1:02:05");
        assert_eq!(parse_hms("0:03:21.500"), Some(201));
        assert_eq!(parse_hms("NOT_IMPLEMENTED"), None);
    }

    #[test]
    fn escaping() {
        assert_eq!(escape("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&apos;");
    }
}
