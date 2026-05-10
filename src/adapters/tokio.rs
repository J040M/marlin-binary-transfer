//! Tokio async adapter — drives the sans-I/O core over an
//! [`AsyncRead`] + [`AsyncWrite`] transport.
//!
//! Mirrors [`adapters::blocking`](crate::adapters::blocking) one-for-one,
//! returning the same [`UploadStats`] / [`UploadError`] types.
//!
//! [`AsyncRead`]: tokio::io::AsyncRead
//! [`AsyncWrite`]: tokio::io::AsyncWrite

use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::adapters::blocking::{UploadError, UploadOptions, UploadStats};
use crate::file_transfer::{Compression, FileEvent, FileTransfer};
use crate::session::Session;

/// Async equivalent of [`adapters::blocking::upload`](crate::adapters::blocking::upload).
pub async fn upload<T, S>(
    transport: &mut T,
    src: &mut S,
    options: UploadOptions,
) -> Result<UploadStats, UploadError>
where
    T: AsyncRead + AsyncWrite + Unpin,
    S: AsyncRead + Unpin,
{
    transport.write_all(b"M28B1\n").await?;

    let mut session = Session::new();
    session.connect(Instant::now());
    drive_until_synced(transport, &mut session).await?;

    let mut ft = FileTransfer::new(&mut session);
    ft.query(options.compression.clone(), Instant::now());
    let negotiated = drive_until_negotiated(transport, &mut ft).await?;

    ft.open(&options.dest_filename, options.dummy, Instant::now());
    drive_until(transport, &mut ft, |e| matches!(e, FileEvent::Opened)).await?;

    let mut stats = UploadStats {
        compression: negotiated.clone(),
        ..UploadStats::default()
    };

    let mut source_bytes = Vec::new();
    src.read_to_end(&mut source_bytes).await?;
    stats.source_bytes = source_bytes.len() as u64;

    let payload: Vec<u8> = match &negotiated {
        Compression::None => source_bytes,
        Compression::Heatshrink { window, lookahead } => {
            #[cfg(feature = "heatshrink")]
            {
                crate::compression::compress(&source_bytes, *window, *lookahead)?
            }
            #[cfg(not(feature = "heatshrink"))]
            {
                let _ = (window, lookahead);
                return Err(UploadError::CompressionFeatureDisabled);
            }
        }
        Compression::Auto => unreachable!("FileTransfer resolves Auto during query"),
    };

    let chunk_size = if options.chunk_size > 0 {
        options.chunk_size
    } else {
        256
    };

    for chunk in payload.chunks(chunk_size) {
        ft.write(chunk, Instant::now());
        drive_until(transport, &mut ft, |e| matches!(e, FileEvent::WriteAcked)).await?;
        stats.bytes_sent += chunk.len() as u64;
        stats.chunks_sent += 1;
    }

    ft.close(Instant::now());
    drive_until(transport, &mut ft, |e| matches!(e, FileEvent::Closed)).await?;

    Ok(stats)
}

async fn drive_until_synced<T>(transport: &mut T, session: &mut Session) -> Result<(), UploadError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    use crate::session::Event;
    let mut buf = [0u8; 1024];
    for _ in 0..200 {
        while let Some(out) = session.poll_outbound() {
            transport.write_all(&out).await?;
        }
        let n = transport.read(&mut buf).await?;
        if n > 0 {
            session.feed(&buf[..n]);
        }
        while let Some(evt) = session.poll_event() {
            if matches!(evt, Event::Synced { .. }) {
                return Ok(());
            }
        }
        session.tick(Instant::now());
    }
    Err(UploadError::HandshakeFailed)
}

async fn drive_until_negotiated<T>(
    transport: &mut T,
    ft: &mut FileTransfer<'_>,
) -> Result<Compression, UploadError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = [0u8; 1024];
    for _ in 0..200 {
        while let Some(out) = ft.poll_outbound() {
            transport.write_all(&out).await?;
        }
        let n = transport.read(&mut buf).await?;
        if n > 0 {
            ft.feed(&buf[..n]);
        }
        while let Some(evt) = ft.poll() {
            match evt {
                FileEvent::Negotiated { compression, .. } => return Ok(compression),
                FileEvent::Failed(err) => return Err(UploadError::Transfer(err)),
                _ => {}
            }
        }
        ft.tick(Instant::now());
    }
    Err(UploadError::Stalled("negotiation did not complete"))
}

async fn drive_until<T, F>(
    transport: &mut T,
    ft: &mut FileTransfer<'_>,
    pred: F,
) -> Result<(), UploadError>
where
    T: AsyncRead + AsyncWrite + Unpin,
    F: Fn(&FileEvent) -> bool,
{
    let mut buf = [0u8; 1024];
    for _ in 0..200 {
        while let Some(out) = ft.poll_outbound() {
            transport.write_all(&out).await?;
        }
        let n = transport.read(&mut buf).await?;
        if n > 0 {
            ft.feed(&buf[..n]);
        }
        while let Some(evt) = ft.poll() {
            if let FileEvent::Failed(err) = &evt {
                return Err(UploadError::Transfer(err.clone()));
            }
            if pred(&evt) {
                return Ok(());
            }
        }
        ft.tick(Instant::now());
    }
    Err(UploadError::Stalled("event did not arrive in time"))
}
