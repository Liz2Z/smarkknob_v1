//! Bounded HTTP/form parsing shared by firmware and host-side tests.
use crate::Parameters;

#[derive(Debug, PartialEq)]
pub enum Error {
    BadRequest,
    Forbidden,
    NotFound,
    MethodNotAllowed,
    TooLarge,
}

impl Error {
    pub fn status(&self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::TooLarge => 413,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Page,
    Status,
    Configure(Parameters, Option<bool>),
    Start(u32),
    Stop,
    Keepalive,
}

pub fn parse(bytes: &[u8]) -> Result<Option<Action>, Error> {
    if bytes.len() > 1536 {
        return Err(Error::TooLarge);
    }
    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let header = core::str::from_utf8(&bytes[..end]).map_err(|_| Error::BadRequest)?;
    let mut lines = header.split("\r\n");
    let mut request = lines.next().ok_or(Error::BadRequest)?.split_whitespace();
    let method = request.next().ok_or(Error::BadRequest)?;
    let path = request.next().ok_or(Error::BadRequest)?;
    let version = request.next().ok_or(Error::BadRequest)?;
    if request.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(Error::BadRequest);
    }
    let mut length = None;
    let mut host = None;
    let mut origin = None;
    let mut form = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Error::BadRequest)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(Error::BadRequest);
            }
            length = Some(value.parse::<usize>().map_err(|_| Error::BadRequest)?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Error::BadRequest);
        } else if name.eq_ignore_ascii_case("host") {
            if host.replace(value).is_some() {
                return Err(Error::BadRequest);
            }
        } else if name.eq_ignore_ascii_case("origin") {
            if origin.replace(value).is_some() {
                return Err(Error::BadRequest);
            }
        } else if name.eq_ignore_ascii_case("content-type") {
            form = value.split(';').next().is_some_and(|v| {
                v.trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            });
        }
    }
    if let Some(origin) = origin {
        let origin_host = origin.strip_prefix("http://").ok_or(Error::Forbidden)?;
        let host = host.ok_or(Error::Forbidden)?;
        if origin_host.strip_suffix(":80").unwrap_or(origin_host)
            != host.strip_suffix(":80").unwrap_or(host)
        {
            return Err(Error::Forbidden);
        }
    }
    let length = length.unwrap_or(0);
    if length > 256 {
        return Err(Error::TooLarge);
    }
    let total = end + 4 + length;
    if bytes.len() < total {
        return Ok(None);
    }
    let body = core::str::from_utf8(&bytes[end + 4..total]).map_err(|_| Error::BadRequest)?;
    match (method, path) {
        ("GET", "/") => Ok(Some(Action::Page)),
        ("GET", "/api/status") => Ok(Some(Action::Status)),
        ("POST", "/api/config") if form => {
            let mut voltage = None;
            let mut frequency = None;
            let mut reverse = None;
            for part in body.split('&') {
                let (key, value) = part.split_once('=').ok_or(Error::BadRequest)?;
                match key {
                    "voltage" if voltage.is_none() => {
                        voltage = Some(value.parse::<f32>().map_err(|_| Error::BadRequest)?)
                    }
                    "frequency_hz" if frequency.is_none() => {
                        frequency = Some(value.parse::<f32>().map_err(|_| Error::BadRequest)?)
                    }
                    "direction" if reverse.is_none() => {
                        reverse = Some(match value {
                            "forward" => false,
                            "reverse" => true,
                            _ => return Err(Error::BadRequest),
                        })
                    }
                    _ => return Err(Error::BadRequest),
                }
            }
            let parameters = Parameters {
                voltage: voltage.ok_or(Error::BadRequest)?,
                frequency_hz: frequency.ok_or(Error::BadRequest)?,
            };
            if !parameters.valid() {
                return Err(Error::BadRequest);
            }
            Ok(Some(Action::Configure(parameters, reverse)))
        }
        ("POST", "/api/start") if body.is_empty() || form => {
            let duration = if body.is_empty() {
                3000
            } else {
                body.strip_prefix("duration_ms=")
                    .ok_or(Error::BadRequest)?
                    .parse::<u32>()
                    .map_err(|_| Error::BadRequest)?
            };
            if duration > 30_000 {
                return Err(Error::BadRequest);
            }
            Ok(Some(Action::Start(duration)))
        }
        ("POST", "/api/stop") if body.is_empty() => Ok(Some(Action::Stop)),
        ("POST", "/api/keepalive") if body.is_empty() => Ok(Some(Action::Keepalive)),
        (
            "GET" | "POST",
            "/api/config" | "/api/start" | "/api/stop" | "/api/keepalive" | "/api/status" | "/",
        ) => Err(Error::MethodNotAllowed),
        ("GET" | "POST", _) => Err(Error::NotFound),
        _ => Err(Error::MethodNotAllowed),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn split_body_is_not_applied_until_complete() {
        let header = b"POST /api/config HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 27\r\n\r\n";
        let mut request = std::vec::Vec::from(header.as_slice());
        request.extend_from_slice(b"voltage=0.7&frequency_hz=");
        assert_eq!(parse(&request), Ok(None));
        request.extend_from_slice(b"12");
        assert_eq!(
            parse(&request),
            Ok(Some(Action::Configure(
                Parameters {
                    voltage: 0.7,
                    frequency_hz: 12.0
                },
                None
            )))
        );
    }

    #[test]
    fn direction_is_optional_explicit_and_not_duplicated() {
        fn request(body: &str) -> std::vec::Vec<u8> {
            let header = std::format!(
                "POST /api/config HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            header.into_bytes()
        }
        assert_eq!(
            parse(&request("voltage=2&frequency_hz=100&direction=reverse")),
            Ok(Some(Action::Configure(
                Parameters {
                    voltage: 2.0,
                    frequency_hz: 100.0
                },
                Some(true)
            )))
        );
        assert_eq!(
            parse(&request("voltage=1&frequency_hz=8&direction=wrong")),
            Err(Error::BadRequest)
        );
        assert_eq!(
            parse(&request(
                "voltage=1&frequency_hz=8&direction=forward&direction=reverse"
            )),
            Err(Error::BadRequest)
        );
    }

    #[test]
    fn rejects_cross_origin_mutation_and_ambiguous_framing() {
        assert_eq!(parse(b"POST /api/stop HTTP/1.1\r\nHost: 192.168.1.5\r\nOrigin: http://evil.invalid\r\n\r\n"), Err(Error::Forbidden));
        assert_eq!(
            parse(b"POST /api/stop HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n"),
            Err(Error::BadRequest)
        );
        assert_eq!(
            parse(b"POST /api/stop HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(Error::BadRequest)
        );
    }

    #[test]
    fn status_and_stop_are_explicit_and_start_defaults_to_three_seconds() {
        assert_eq!(
            parse(b"GET /api/status HTTP/1.1\r\n\r\n"),
            Ok(Some(Action::Status))
        );
        assert_eq!(
            parse(b"POST /api/stop HTTP/1.1\r\n\r\n"),
            Ok(Some(Action::Stop))
        );
        assert_eq!(
            parse(b"POST /api/start HTTP/1.1\r\n\r\n"),
            Ok(Some(Action::Start(3000)))
        );
        assert_eq!(
            parse(b"GET /api/start HTTP/1.1\r\n\r\n"),
            Err(Error::MethodNotAllowed)
        );
    }

    #[test]
    fn rejects_unbounded_body_and_non_finite_parameters() {
        assert_eq!(
            parse(b"POST /api/config HTTP/1.1\r\nContent-Length: 257\r\n\r\n"),
            Err(Error::TooLarge)
        );
        assert_eq!(parse(b"POST /api/config HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 26\r\n\r\nvoltage=NaN&frequency_hz=8"), Err(Error::BadRequest));
        assert!(
            !Parameters {
                voltage: f32::NAN,
                frequency_hz: 8.0
            }
            .valid()
        );
    }
}
