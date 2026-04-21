use mail_parser::{MessageParser, MimeHeaders};
use zeroclaw::channels::SendMessage;
use zeroclaw::channels::media_pipeline::MediaAttachment;

/// Test that extract_attachments correctly parses binary attachments from multipart MIME
#[test]
fn extract_attachments_from_multipart_email() {
    // Construct a raw multipart MIME email with a PDF and an image attachment
    let raw_email = concat!(
        "From: sender@example.com\r\n",
        "To: recipient@example.com\r\n",
        "Subject: Test with attachments\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"BOUNDARY\"\r\n",
        "\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "Email body text\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: application/pdf\r\n",
        "Content-Disposition: attachment; filename=\"document.pdf\"\r\n",
        "\r\n",
        "PDF_BINARY_DATA\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: image/png\r\n",
        "Content-Disposition: attachment; filename=\"photo.png\"\r\n",
        "\r\n",
        "PNG_BINARY_DATA\r\n",
        "--BOUNDARY--\r\n"
    );

    let parsed = MessageParser::default()
        .parse(raw_email.as_bytes())
        .unwrap();

    // Call the helper method we're about to implement
    let attachments = extract_attachments_helper(&parsed);

    // Should have 2 attachments (PDF and PNG, not text/plain)
    assert_eq!(attachments.len(), 2);

    let pdf = attachments.iter().find(|a| a.file_name == "document.pdf");
    assert!(pdf.is_some());
    let pdf = pdf.unwrap();
    assert_eq!(pdf.mime_type.as_deref(), Some("application/pdf"));
    assert_eq!(pdf.data, b"PDF_BINARY_DATA");

    let png = attachments.iter().find(|a| a.file_name == "photo.png");
    assert!(png.is_some());
    let png = png.unwrap();
    assert_eq!(png.mime_type.as_deref(), Some("image/png"));
    assert_eq!(png.data, b"PNG_BINARY_DATA");
}

/// Test that text parts are skipped by extract_attachments
#[test]
fn extract_attachments_skips_text_parts() {
    let raw_email = concat!(
        "From: sender@example.com\r\n",
        "To: recipient@example.com\r\n",
        "Subject: Text only\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"BOUNDARY\"\r\n",
        "\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "Plain text body\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: text/html\r\n",
        "\r\n",
        "<html><body>HTML body</body></html>\r\n",
        "--BOUNDARY--\r\n"
    );

    let parsed = MessageParser::default()
        .parse(raw_email.as_bytes())
        .unwrap();
    let attachments = extract_attachments_helper(&parsed);

    // No binary attachments (text/plain and text/html are skipped)
    assert_eq!(attachments.len(), 0);
}

/// Test that extract_attachments respects max_attachment_bytes size limit
#[test]
fn extract_attachments_respects_size_limit() {
    let raw_email = concat!(
        "From: sender@example.com\r\n",
        "To: recipient@example.com\r\n",
        "Subject: Large attachment\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"BOUNDARY\"\r\n",
        "\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "Body\r\n",
        "--BOUNDARY\r\n",
        "Content-Type: application/octet-stream\r\n",
        "Content-Disposition: attachment; filename=\"large.bin\"\r\n",
        "\r\n",
    );

    // Append large data exceeding 100 bytes
    let mut full_email = raw_email.to_string();
    full_email.push_str(&"X".repeat(150));
    full_email.push_str("\r\n--BOUNDARY--\r\n");

    let parsed = MessageParser::default()
        .parse(full_email.as_bytes())
        .unwrap();

    // With 100-byte limit, the 150-byte attachment should be dropped
    let attachments = extract_attachments_with_limit(&parsed, 100);
    assert_eq!(attachments.len(), 0);

    // With 200-byte limit, the 150-byte attachment should be included
    let attachments = extract_attachments_with_limit(&parsed, 200);
    assert_eq!(attachments.len(), 1);
}

/// Test SendMessage::new() initializes attachments to empty vec
#[test]
fn send_message_attachments_default_empty() {
    let msg = SendMessage::new("content", "recipient@example.com");
    assert!(msg.attachments.is_empty());
}

/// Test SendMessage::with_attachments() builder method
#[test]
fn send_message_with_attachments_builder() {
    let attachments = vec![MediaAttachment {
        file_name: "test.pdf".to_string(),
        data: vec![1, 2, 3],
        mime_type: Some("application/pdf".to_string()),
    }];

    let msg =
        SendMessage::new("content", "recipient@example.com").with_attachments(attachments.clone());

    assert_eq!(msg.attachments.len(), 1);
    assert_eq!(msg.attachments[0].file_name, "test.pdf");
}

// Helper functions that mimic the methods we'll implement on EmailChannel

fn extract_attachments_helper(parsed: &mail_parser::Message) -> Vec<MediaAttachment> {
    extract_attachments_with_limit(parsed, 25 * 1024 * 1024)
}

fn extract_attachments_with_limit(
    parsed: &mail_parser::Message,
    max_bytes: usize,
) -> Vec<MediaAttachment> {
    let mut attachments = Vec::new();
    let mut total_size = 0;

    for part in parsed.attachments() {
        let part: &mail_parser::MessagePart = part;
        let ct = MimeHeaders::content_type(part);
        let mime_str =
            ct.map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")));

        // Skip text parts — already handled by extract_text()
        if let Some(ref m) = mime_str {
            if m.starts_with("text/") {
                continue;
            }
        }

        let data = part.contents().to_vec();
        if data.is_empty() {
            continue;
        }

        // Check size limit
        total_size += data.len();
        if total_size > max_bytes {
            break;
        }

        let file_name = MimeHeaders::attachment_name(part)
            .unwrap_or("attachment")
            .to_string();

        attachments.push(MediaAttachment {
            file_name,
            data,
            mime_type: mime_str,
        });
    }
    attachments
}
