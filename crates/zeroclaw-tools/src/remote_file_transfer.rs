//! Transfer ordering shared by Remote Files backends.

use std::io;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Copy up to the configured byte limit into a temporary destination.
/// An excess byte is detected before it is written.
pub async fn copy_bounded(
    mut source: impl AsyncRead + Unpin,
    mut staged_destination: impl AsyncWrite + Unpin,
    limit: u64,
) -> io::Result<u64> {
    let mut buffer = [0; 16 * 1024];
    let mut written = 0;
    loop {
        let remaining = limit - written;
        let capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let count = source.read(&mut buffer[..capacity]).await?;
        if count == 0 {
            staged_destination.flush().await?;
            return Ok(written);
        }
        if count as u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "remote_files_byte_limit",
            ));
        }
        staged_destination.write_all(&buffer[..count]).await?;
        written += count as u64;
    }
}

/// A backend bound to the already authorized source and destination.
/// The remote servers remain authoritative for their file permissions.
#[async_trait]
pub trait StagedTransfer {
    /// Publish the verified staging object. On an ambiguous result, return
    /// Unknown and preserve the source for operator reconciliation.
    async fn publish(&mut self) -> PublishResult;

    /// Must revalidate live rights and ensure the source still identifies the
    /// object that was copied. If either check fails, leave the source intact.
    async fn remove_verified_source(&mut self) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishResult {
    Confirmed,
    Rejected,
    Unknown,
}

#[derive(Debug)]
pub enum TransferResult {
    Copied,
    Moved,
    DestinationRejected,
    DestinationUnconfirmed,
    CopiedSourceRemovalUnconfirmed(io::Error),
}

/// Finish a staged transfer after the adapter verifies its integrity. The
/// adapter owns staging cleanup and must never treat a lost response as
/// confirmation. A successful copy does not require source removal.
pub async fn finish_staged_transfer(
    transfer: &mut impl StagedTransfer,
    remove_source: bool,
) -> TransferResult {
    match transfer.publish().await {
        PublishResult::Rejected => return TransferResult::DestinationRejected,
        PublishResult::Unknown => return TransferResult::DestinationUnconfirmed,
        PublishResult::Confirmed => {}
    }

    if !remove_source {
        return TransferResult::Copied;
    }
    match transfer.remove_verified_source().await {
        Ok(()) => TransferResult::Moved,
        Err(error) => TransferResult::CopiedSourceRemovalUnconfirmed(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        publish: PublishResult,
        remove_allowed: bool,
        calls: Vec<&'static str>,
    }

    #[async_trait]
    impl StagedTransfer for Fixture {
        async fn publish(&mut self) -> PublishResult {
            self.calls.push("publish");
            self.publish
        }

        async fn remove_verified_source(&mut self) -> io::Result<()> {
            self.calls.push("remove");
            if self.remove_allowed {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        }
    }

    #[tokio::test]
    async fn source_removal_follows_only_a_confirmed_publication() {
        for publish in [PublishResult::Rejected, PublishResult::Unknown] {
            let mut fixture = Fixture {
                publish,
                remove_allowed: true,
                calls: Vec::new(),
            };
            let result = finish_staged_transfer(&mut fixture, true).await;
            assert!(matches!(
                result,
                TransferResult::DestinationRejected | TransferResult::DestinationUnconfirmed
            ));
            assert_eq!(fixture.calls, ["publish"]);
        }

        let mut fixture = Fixture {
            publish: PublishResult::Confirmed,
            remove_allowed: true,
            calls: Vec::new(),
        };
        assert!(matches!(
            finish_staged_transfer(&mut fixture, true).await,
            TransferResult::Moved
        ));
        assert_eq!(fixture.calls, ["publish", "remove"]);
    }

    #[tokio::test]
    async fn denied_removal_reports_partial_move_and_copy_never_removes() {
        let mut fixture = Fixture {
            publish: PublishResult::Confirmed,
            remove_allowed: false,
            calls: Vec::new(),
        };
        assert!(matches!(
            finish_staged_transfer(&mut fixture, true).await,
            TransferResult::CopiedSourceRemovalUnconfirmed(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        fixture.calls.clear();
        assert!(matches!(
            finish_staged_transfer(&mut fixture, false).await,
            TransferResult::Copied
        ));
        assert_eq!(fixture.calls, ["publish"]);
    }

    #[tokio::test]
    async fn streaming_accepts_exact_limit_and_does_not_write_excess() {
        let mut destination = Vec::new();
        assert_eq!(
            copy_bounded(&b"four"[..], &mut destination, 4)
                .await
                .unwrap(),
            4
        );
        assert_eq!(destination, b"four");
        destination.clear();
        let error = copy_bounded(&b"five!"[..], &mut destination, 4)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(destination.len() <= 4);
    }
}
