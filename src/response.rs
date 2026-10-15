use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

use crate::error::ApiError;
use crate::params::Params;

pub const API_VERSION: &str = "1.16.1";
pub const SERVER_TYPE: &str = "juuz";

#[derive(Clone, Debug)]
pub enum Format {
    Xml,
    Json,
    Jsonp(String),
}

impl Format {
    pub fn from_params(p: &Params) -> Format {
        match p.get("f") {
            Some("json") => Format::Json,
            Some("jsonp") => match p.get("callback") {
                Some(cb) if valid_callback(cb) => Format::Jsonp(cb.to_string()),
                _ => Format::Json,
            },
            _ => Format::Xml,
        }
    }
}

fn valid_callback(cb: &str) -> bool {
    !cb.is_empty()
        && cb
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
}

pub fn envelope(payload: Result<Value, &ApiError>) -> Value {
    let mut m = Map::new();
    match &payload {
        Ok(_) => m.insert("status".into(), json!("ok")),
        Err(_) => m.insert("status".into(), json!("failed")),
    };
    m.insert("version".into(), json!(API_VERSION));
    m.insert("type".into(), json!(SERVER_TYPE));
    m.insert("serverVersion".into(), json!(env!("CARGO_PKG_VERSION")));
    m.insert("openSubsonic".into(), json!(true));
    match payload {
        Ok(Value::Object(extra)) => {
            for (k, v) in extra {
                m.insert(k, v);
            }
        }
        Ok(_) => {}
        Err(e) => {
            m.insert(
                "error".into(),
                json!({"code": e.code, "message": e.message}),
            );
        }
    }
    Value::Object(m)
}

pub fn render(fmt: &Format, payload: Result<Value, &ApiError>) -> Response {
    let body = envelope(payload);
    match fmt {
        Format::Json => {
            let text = json!({"subsonic-response": body}).to_string();
            respond("application/json; charset=utf-8", text)
        }
        Format::Jsonp(cb) => {
            let text = format!("{cb}({})", json!({"subsonic-response": body}));
            respond("application/javascript; charset=utf-8", text)
        }
        Format::Xml => {
            let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
            if let Value::Object(mut m) = body {
                m.insert("xmlns".into(), json!("http://subsonic.org/restapi"));
                write_obj(&mut out, "subsonic-response", &m);
            }
            respond("application/xml; charset=utf-8", out)
        }
    }
}

fn respond(content_type: &'static str, body: String) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    resp
}

fn is_scalar(v: &Value) -> bool {
    matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
}

fn write_obj(out: &mut String, name: &str, obj: &Map<String, Value>) {
    out.push('<');
    out.push_str(name);
    for (k, v) in obj {
        if k != "value" && is_scalar(v) {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            escape(&scalar_text(v), out);
            out.push('"');
        }
    }

    let text = obj.get("value").filter(|v| is_scalar(v));
    let has_children = obj
        .iter()
        .any(|(k, v)| k != "value" && matches!(v, Value::Object(_) | Value::Array(_)));

    if text.is_none() && !has_children {
        out.push_str("/>");
        return;
    }
    out.push('>');
    if let Some(t) = text {
        escape(&scalar_text(t), out);
    }
    for (k, v) in obj {
        if k == "value" {
            continue;
        }
        match v {
            Value::Object(o) => write_obj(out, k, o),
            Value::Array(items) => {
                for item in items {
                    match item {
                        Value::Object(o) => write_obj(out, k, o),
                        v if is_scalar(v) => {
                            out.push('<');
                            out.push_str(k);
                            out.push('>');
                            escape(&scalar_text(v), out);
                            out.push_str("</");
                            out.push_str(k);
                            out.push('>');
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}
