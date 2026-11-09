//! Minimal TLS ClientHello reader.
//!
//! The edge needs one field, the SNI, and it needs the original bytes intact.
//! It does not build a TLS session, choose a certificate, or send a ServerHello.
//! The caller buffers what this parser reads and forwards it to the backend
//! unchanged.

use std::fmt;

/// Largest ClientHello the edge will buffer while looking for SNI.
pub const MAX_CLIENT_HELLO: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelloParse {
    /// More bytes are needed before the handshake header or SNI is complete.
    NeedMore,
    /// The ClientHello is complete. `None` means it carried no SNI.
    Complete(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelloError {
    TooLarge,
    Malformed(&'static str),
    Unsupported(&'static str),
}

impl fmt::Display for HelloError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("client hello exceeds the buffer budget"),
            Self::Malformed(reason) => write!(f, "malformed client hello: {reason}"),
            Self::Unsupported(reason) => write!(f, "unsupported client hello: {reason}"),
        }
    }
}

impl std::error::Error for HelloError {}

/// Parse enough of a TLS record stream to find the ClientHello SNI.
///
pub fn parse_server_name(bytes: &[u8]) -> Result<HelloParse, HelloError> {
    let mut offset = 0;
    let mut handshake = Vec::new();

    loop {
        if bytes.len().saturating_sub(offset) < 5 {
            return Ok(HelloParse::NeedMore);
        }
        if bytes[offset] != 22 {
            return Err(HelloError::Malformed("first record is not a handshake"));
        }
        let length = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]) as usize;
        if length > 16 * 1024 {
            return Err(HelloError::Malformed("record exceeds the TLS record limit"));
        }
        if bytes.len().saturating_sub(offset + 5) < length {
            return Ok(HelloParse::NeedMore);
        }
        handshake.extend_from_slice(&bytes[offset + 5..offset + 5 + length]);
        offset += 5 + length;

        if handshake.len() >= 4 {
            if handshake[0] != 1 {
                return Err(HelloError::Malformed("first handshake is not ClientHello"));
            }
            let body_length = u24(&handshake[1..4]);
            if body_length > MAX_CLIENT_HELLO {
                return Err(HelloError::TooLarge);
            }
            if handshake.len() >= 4 + body_length {
                return parse_client_hello(&handshake[4..4 + body_length]);
            }
        }
    }
}

fn parse_client_hello(body: &[u8]) -> Result<HelloParse, HelloError> {
    let mut cursor = Cursor::new(body);
    cursor.skip(2, "legacy version")?;
    cursor.skip(32, "random")?;
    cursor.skip_u8("session id")?;
    cursor.skip_u16("cipher suites")?;
    cursor.skip_u8("compression methods")?;
    if cursor.remaining() == 0 {
        return Ok(HelloParse::Complete(None));
    }

    let extensions = cursor.length_prefixed_u16("extensions")?;
    let mut cursor = Cursor::new(extensions);
    while cursor.remaining() >= 4 {
        let kind = cursor.u16("extension type")?;
        let data = cursor.length_prefixed_u16("extension data")?;
        if kind == 0 {
            return parse_server_name_extension(data);
        }
    }
    Ok(HelloParse::Complete(None))
}

fn parse_server_name_extension(data: &[u8]) -> Result<HelloParse, HelloError> {
    let mut cursor = Cursor::new(data);
    let list = cursor.length_prefixed_u16("server name list")?;
    let mut list = Cursor::new(list);
    while list.remaining() >= 3 {
        let name_type = list.u8("server name type")?;
        let name = list.length_prefixed_u16("server name")?;
        if name_type != 0 {
            continue;
        }
        let name = std::str::from_utf8(name)
            .map_err(|_| HelloError::Malformed("SNI is not UTF-8"))?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if name.is_empty()
            || name.len() > 255
            || !name.is_ascii()
            || name
                .chars()
                .any(|c| c.is_ascii_whitespace() || c.is_ascii_control() || c == '/' || c == '\\')
        {
            return Err(HelloError::Malformed("SNI is not a valid hostname"));
        }
        return Ok(HelloParse::Complete(Some(name)));
    }
    Ok(HelloParse::Complete(None))
}

fn u24(bytes: &[u8]) -> usize {
    ((bytes[0] as usize) << 16) | ((bytes[1] as usize) << 8) | bytes[2] as usize
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, length: usize, field: &'static str) -> Result<&'a [u8], HelloError> {
        if self.remaining() < length {
            return Err(HelloError::Malformed(field));
        }
        let value = &self.bytes[self.offset..self.offset + length];
        self.offset += length;
        Ok(value)
    }

    fn skip(&mut self, length: usize, field: &'static str) -> Result<(), HelloError> {
        self.take(length, field).map(|_| ())
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, HelloError> {
        Ok(self.take(1, field)?[0])
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, HelloError> {
        let bytes = self.take(2, field)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn skip_u8(&mut self, field: &'static str) -> Result<(), HelloError> {
        let length = self.u8(field)? as usize;
        self.skip(length, field)
    }

    fn skip_u16(&mut self, field: &'static str) -> Result<(), HelloError> {
        let length = self.u16(field)? as usize;
        self.skip(length, field)
    }

    fn length_prefixed_u16(&mut self, field: &'static str) -> Result<&'a [u8], HelloError> {
        let length = self.u16(field)? as usize;
        self.take(length, field)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustls::pki_types::ServerName;

    use super::*;

    fn client_hello(name: &str) -> Vec<u8> {
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let name = ServerName::try_from(name.to_string()).unwrap();
        let mut connection = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
        let mut bytes = Vec::new();
        connection.write_tls(&mut bytes).unwrap();
        assert!(!bytes.is_empty());
        bytes
    }

    #[test]
    fn reads_sni_from_a_real_client_hello() {
        let bytes = client_hello("confluence.soil");
        assert_eq!(
            parse_server_name(&bytes).unwrap(),
            HelloParse::Complete(Some("confluence.soil".into()))
        );
    }

    #[test]
    fn normalizes_case_and_trailing_dot() {
        let bytes = client_hello("Confluence.Soil.");
        assert_eq!(
            parse_server_name(&bytes).unwrap(),
            HelloParse::Complete(Some("confluence.soil".into()))
        );
    }

    #[test]
    fn waits_for_a_fragmented_hello() {
        let bytes = client_hello("confluence.soil");
        assert_eq!(
            parse_server_name(&bytes[..3]).unwrap(),
            HelloParse::NeedMore
        );
        assert_eq!(
            parse_server_name(&bytes[..bytes.len() - 5]).unwrap(),
            HelloParse::NeedMore
        );
    }

    #[test]
    fn rejects_non_tls_and_bad_framing() {
        assert!(parse_server_name(&[23, 3, 3, 0, 1, 0]).is_err());
        let mut bytes = client_hello("confluence.soil");
        bytes[0] = 0x17;
        assert!(parse_server_name(&bytes).is_err());
        let mut bytes = client_hello("confluence.soil");
        bytes[5] = 0x02;
        assert!(parse_server_name(&bytes).is_err());
    }
}
