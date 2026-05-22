use core::fmt::Write;
use heapless::String;

/// Write a successful JSON response into `buf`.
///
/// Format: `{"id":"<id>","ok":true,"result":"<result>"}`
pub fn write_ok<const N: usize>(buf: &mut String<N>, id: &str, result: &str) {
    buf.clear();
    let _ = write!(buf, "{{\"id\":\"{}\",\"ok\":true,\"result\":\"{}\"}}", id, result);
}

/// Write an error JSON response into `buf`.
///
/// Format: `{"id":"<id>","ok":false,"result":"","error":"<error>"}`
pub fn write_err<const N: usize>(buf: &mut String<N>, id: &str, error: &str) {
    buf.clear();
    let _ = write!(
        buf,
        "{{\"id\":\"{}\",\"ok\":false,\"result\":\"\",\"error\":\"{}\"}}",
        id, error
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_ok_response() {
        let mut buf = String::<128>::new();
        write_ok(&mut buf, "42", "pong");
        assert_eq!(buf.as_str(), r#"{"id":"42","ok":true,"result":"pong"}"#);
    }

    #[test]
    fn write_ok_done() {
        let mut buf = String::<128>::new();
        write_ok(&mut buf, "1", "done");
        assert_eq!(buf.as_str(), r#"{"id":"1","ok":true,"result":"done"}"#);
    }

    #[test]
    fn write_error_response() {
        let mut buf = String::<128>::new();
        write_err(&mut buf, "42", "Invalid pin -1");
        assert_eq!(
            buf.as_str(),
            r#"{"id":"42","ok":false,"result":"","error":"Invalid pin -1"}"#
        );
    }

    #[test]
    fn write_ok_clears_buffer() {
        let mut buf = String::<128>::new();
        let _ = write!(buf, "garbage");
        write_ok(&mut buf, "1", "pong");
        assert_eq!(buf.as_str(), r#"{"id":"1","ok":true,"result":"pong"}"#);
    }

    #[test]
    fn write_err_unknown_command() {
        let mut buf = String::<128>::new();
        write_err(&mut buf, "99", "Unknown command");
        assert_eq!(
            buf.as_str(),
            r#"{"id":"99","ok":false,"result":"","error":"Unknown command"}"#
        );
    }
}
