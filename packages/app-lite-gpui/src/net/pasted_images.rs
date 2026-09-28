//! Bytes for images inside HTML pasted from another app. Evernote keeps such
//! images as note resources rather than hot-linking them; so do we.
//!
//! Downloads run on a few worker threads, stream each image to a staging
//! file in bounded chunks, and hand every result back as soon as it is
//! ready. A paste has a total byte budget and one overall deadline, and is
//! cancelled when its note session goes away.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::native_editor::images::ResourceImport;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";
const CHUNK_BYTES: usize = 64 * 1024;
/// Network downloads in flight across every paste in this process.
const MAX_CONCURRENT_DOWNLOADS: usize = 6;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PasteDownloadLimits {
    /// Images past this many stay as links to their source.
    pub max_images: usize,
    pub image_bytes: usize,
    /// All images of one paste together.
    pub total_bytes: u64,
    /// For the whole paste, not per image.
    pub deadline: Duration,
    pub connect_timeout: Duration,
    pub workers: usize,
}

impl Default for PasteDownloadLimits {
    fn default() -> Self {
        Self {
            max_images: 200,
            image_bytes: app_lite_core::MAX_IMAGE_BYTES,
            total_bytes: 200 * 1024 * 1024,
            deadline: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(10),
            workers: 4,
        }
    }
}

/// A running paste download. Dropping it cancels what has not finished:
/// requests in flight are abandoned at once (their connections close), no
/// matter how long a silent server would keep them waiting.
pub(crate) struct PasteDownload {
    cancel: tokio::sync::watch::Sender<bool>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    #[cfg(test)]
    staging: Option<std::path::PathBuf>,
}

impl PasteDownload {
    pub(crate) fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    /// Blocks until every result is delivered and the staging directory
    /// is gone.
    pub(crate) fn wait(&self) {
        let (done, changed) = &*self.finished;
        let mut done = done.lock().expect("download mutex");
        while !*done {
            done = changed.wait(done).expect("download mutex");
        }
    }

    /// `wait` for at most `limit`; false when the download is still running.
    #[cfg(test)]
    pub(crate) fn wait_for(&self, limit: Duration) -> bool {
        let (done, changed) = &*self.finished;
        let done = done.lock().expect("download mutex");
        let (done, _) = changed
            .wait_timeout_while(done, limit, |done| !*done)
            .expect("download mutex");
        *done
    }

    #[cfg(test)]
    pub(crate) fn staging_path(&self) -> Option<&Path> {
        self.staging.as_deref()
    }
}

impl Drop for PasteDownload {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct Batch {
    sources: Vec<(String, String)>,
    limits: PasteDownloadLimits,
    started: Instant,
    next: AtomicUsize,
    staged_bytes: AtomicU64,
    cancel: tokio::sync::watch::Receiver<bool>,
    staging: Option<tempfile::TempDir>,
    client: Option<reqwest::Client>,
    /// For this machine's own addresses, which no proxy should see.
    direct_client: Option<reqwest::Client>,
    network_slots: Arc<tokio::sync::Semaphore>,
}

impl Batch {
    fn remaining(&self) -> Option<Duration> {
        self.limits
            .deadline
            .checked_sub(self.started.elapsed())
            .filter(|left| !left.is_zero())
    }

    fn check(&self) -> Result<(), String> {
        if *self.cancel.borrow() {
            return Err("已取消".to_owned());
        }
        if self.remaining().is_none() {
            return Err("下载图片超时".to_owned());
        }
        Ok(())
    }
}

/// Network downloads in flight across every paste in this process.
fn network_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS))),
    )
}

/// Whether a failed fetch may succeed on a later try: cut short, timed out,
/// a network failure or a server error, not a lasting answer about the image.
pub(crate) fn is_retryable(error: &str) -> bool {
    match error {
        "已取消"
        | "下载图片超时"
        | "无法建立网络连接"
        | "无法启动下载"
        | "无法准备图片暂存目录" => true,
        error if error.starts_with("下载图片失败：HTTP ") => error
            .trim_start_matches("下载图片失败：HTTP ")
            .starts_with('5'),
        error => {
            error.starts_with("下载图片失败：")
                || error.starts_with("无法写入图片暂存文件")
                || error.starts_with("保存图片到笔记失败：")
        }
    }
}

/// Starts fetching `sources` (`(src, alt)` pairs) and calls `deliver` once
/// per source, from the download thread, as each one is ready or fails.
pub(crate) fn start_pasted_image_downloads(
    sources: Vec<(String, String)>,
    limits: PasteDownloadLimits,
    deliver: impl Fn(usize, Result<ResourceImport, String>) + Send + Sync + 'static,
) -> PasteDownload {
    start_with_slots(sources, limits, network_slots(), deliver)
}

fn start_with_slots(
    sources: Vec<(String, String)>,
    limits: PasteDownloadLimits,
    network_slots: Arc<tokio::sync::Semaphore>,
    deliver: impl Fn(usize, Result<ResourceImport, String>) + Send + Sync + 'static,
) -> PasteDownload {
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let workers = limits.workers.clamp(1, 16).min(sources.len().max(1));
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    let needs_network = sources
        .iter()
        .any(|(source, _)| strip_ascii_prefix(source, "http").is_some());
    let build = |direct: bool| {
        needs_network
            .then(|| {
                super::default_image_request_headers(USER_AGENT)
                    .ok()
                    .and_then(|headers| {
                        let builder = reqwest::Client::builder()
                            .connect_timeout(limits.connect_timeout)
                            .redirect(reqwest::redirect::Policy::limited(10))
                            .default_headers(headers);
                        if direct { builder.no_proxy() } else { builder }
                            .build()
                            .ok()
                    })
            })
            .flatten()
    };
    let (client, direct_client) = (build(false), build(true));
    let staging = tempfile::Builder::new()
        .prefix("joplin-lite-paste-")
        .tempdir()
        .ok();
    #[cfg(test)]
    let staging_path = staging.as_ref().map(|dir| dir.path().to_owned());
    let batch = Arc::new(Batch {
        sources,
        limits,
        started: Instant::now(),
        next: AtomicUsize::new(0),
        staged_bytes: AtomicU64::new(0),
        cancel: cancelled,
        staging,
        client,
        direct_client,
        network_slots,
    });
    let signal = Arc::clone(&finished);
    std::thread::spawn(move || {
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            let deliver = &deliver;
            runtime.block_on(futures::future::join_all(
                (0..workers).map(|_| run_worker(&batch, deliver)),
            ));
        } else {
            for index in 0..batch.sources.len() {
                deliver(index, Err("无法启动下载".to_owned()));
            }
        }
        // The staging directory goes before anyone waiting is told.
        drop(batch);
        let (done, changed) = &*signal;
        *done.lock().expect("download mutex") = true;
        changed.notify_all();
    });
    PasteDownload {
        cancel,
        finished,
        #[cfg(test)]
        staging: staging_path,
    }
}

async fn run_worker(
    batch: &Batch,
    deliver: &(impl Fn(usize, Result<ResourceImport, String>) + Send + Sync),
) {
    loop {
        let index = batch.next.fetch_add(1, Ordering::SeqCst);
        let Some((source, alt)) = batch.sources.get(index) else {
            break;
        };
        let result = if index >= batch.limits.max_images {
            Err(format!("一次最多粘贴 {} 张图片", batch.limits.max_images))
        } else {
            match batch.check() {
                Ok(()) => bounded(batch, stage_image(batch, index, source, alt)).await,
                Err(error) => Err(error),
            }
        };
        deliver(index, result);
    }
}

/// Runs `work` until it finishes, the paste is cancelled or its deadline
/// passes; the latter two drop it mid-request.
async fn bounded<T>(
    batch: &Batch,
    work: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let mut cancelled = batch.cancel.clone();
    let cancel = async move {
        while !*cancelled.borrow_and_update() {
            if cancelled.changed().await.is_err() {
                // The download handle is gone: that is a cancel too.
                break;
            }
        }
    };
    let remaining = batch.remaining().unwrap_or_default();
    match futures::future::select(
        std::pin::pin!(tokio::time::timeout(remaining, work)),
        std::pin::pin!(cancel),
    )
    .await
    {
        futures::future::Either::Left((Ok(result), _)) => result,
        futures::future::Either::Left((Err(_), _)) => Err("下载图片超时".to_owned()),
        futures::future::Either::Right(_) => Err("已取消".to_owned()),
    }
}

/// Blocking form: every result, in source order.
pub(crate) fn fetch_pasted_images(
    sources: &[(String, String)],
) -> Vec<Result<ResourceImport, String>> {
    let results = Arc::new(Mutex::new(
        (0..sources.len()).map(|_| None).collect::<Vec<_>>(),
    ));
    let sink = Arc::clone(&results);
    let download = start_pasted_image_downloads(
        sources.to_vec(),
        PasteDownloadLimits::default(),
        move |index, result| sink.lock().expect("results")[index] = Some(result),
    );
    download.wait();
    let mut results = results.lock().expect("results");
    results
        .iter_mut()
        .map(|result| result.take().unwrap_or_else(|| Err("已取消".to_owned())))
        .collect()
}

async fn stage_image(
    batch: &Batch,
    index: usize,
    source: &str,
    alt: &str,
) -> Result<ResourceImport, String> {
    let staging = batch
        .staging
        .as_ref()
        .ok_or_else(|| "无法准备图片暂存目录".to_owned())?;
    let partial = staging.path().join(format!("{index}.part"));
    let mut file = std::fs::File::create(&partial)
        .map_err(|error| format!("无法写入图片暂存文件：{error}"))?;
    let mut written = 0_usize;
    let name = if let Some(data) = strip_ascii_prefix(source, "data:") {
        let bytes = decode_data_uri(data)?;
        for chunk in bytes.chunks(CHUNK_BYTES) {
            write_chunk(batch, &mut file, &mut written, chunk)?;
        }
        None
    } else {
        let url = url::Url::parse(source).map_err(|_| "图片地址无效".to_owned())?;
        let name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(percent_decode)
            .filter(|name| name.contains('.') && !name.starts_with('.'));
        match url.scheme() {
            "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| "图片文件路径无效".to_owned())?;
                let mut source = std::fs::File::open(&path)
                    .map_err(|error| format!("无法读取图片文件：{error}"))?;
                let mut buffer = vec![0_u8; CHUNK_BYTES];
                loop {
                    let read = match source.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => read,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(format!("读取图片失败：{error}")),
                    };
                    write_chunk(batch, &mut file, &mut written, &buffer[..read])?;
                }
            }
            "http" | "https" => {
                let loopback = match url.host() {
                    Some(url::Host::Ipv4(address)) => address.is_loopback(),
                    Some(url::Host::Ipv6(address)) => address.is_loopback(),
                    Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
                    None => false,
                };
                let client = if loopback {
                    &batch.direct_client
                } else {
                    &batch.client
                }
                .as_ref()
                .ok_or_else(|| "无法建立网络连接".to_owned())?;
                let _slot = batch
                    .network_slots
                    .acquire()
                    .await
                    .map_err(|_| "无法建立网络连接".to_owned())?;
                let mut response = client
                    .get(url.as_str())
                    .send()
                    .await
                    .map_err(|error| format!("下载图片失败：{error}"))?;
                if !response.status().is_success() {
                    return Err(format!("下载图片失败：HTTP {}", response.status().as_u16()));
                }
                if response
                    .content_length()
                    .is_some_and(|length| length > batch.limits.image_bytes as u64)
                {
                    return Err("图片太大".to_owned());
                }
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|error| format!("读取图片失败：{error}"))?
                {
                    write_chunk(batch, &mut file, &mut written, &chunk)?;
                }
            }
            _ => return Err("不支持的图片地址".to_owned()),
        }
        name
    };
    drop(file);
    let extension = sniff_extension(&partial)?;
    let staged = staging.path().join(format!("{index}.{extension}"));
    std::fs::rename(&partial, &staged).map_err(|error| format!("无法暂存图片：{error}"))?;
    let mut import = ResourceImport::from_path(&staged).map_err(|error| error.to_string())?;
    if !import.is_image() {
        return Err("不是可识别的图片".to_owned());
    }
    import.title = name
        .or_else(|| {
            let alt = alt.trim();
            (!alt.is_empty() && alt.chars().count() <= 120).then(|| format!("{alt}.{extension}"))
        })
        .unwrap_or_else(|| format!("图片.{extension}"));
    Ok(import)
}

/// Stops at the image's own limit or the paste's total budget.
fn write_chunk(
    batch: &Batch,
    file: &mut std::fs::File,
    written: &mut usize,
    chunk: &[u8],
) -> Result<(), String> {
    batch.check()?;
    *written += chunk.len();
    if *written > batch.limits.image_bytes {
        return Err("图片太大".to_owned());
    }
    let total = batch
        .staged_bytes
        .fetch_add(chunk.len() as u64, Ordering::SeqCst)
        + chunk.len() as u64;
    if total > batch.limits.total_bytes {
        return Err("这次粘贴的图片总量超过上限".to_owned());
    }
    file.write_all(chunk)
        .map_err(|error| format!("无法写入图片暂存文件：{error}"))
}

fn sniff_extension(path: &Path) -> Result<&'static str, String> {
    let mut head = [0_u8; 64];
    let read = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .map_err(|error| format!("无法读取图片暂存文件：{error}"))?;
    Ok(match image::guess_format(&head[..read]) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::Gif) => "gif",
        Ok(image::ImageFormat::WebP) => "webp",
        Ok(image::ImageFormat::Bmp) => "bmp",
        Ok(image::ImageFormat::Tiff) => "tiff",
        _ => return Err("不是可识别的图片".to_owned()),
    })
}

fn decode_data_uri(data: &str) -> Result<Vec<u8>, String> {
    let (header, payload) = data
        .split_once(',')
        .ok_or_else(|| "图片数据无效".to_owned())?;
    let too_large = || "图片太大".to_owned();
    // The limit is on decoded bytes, counted exactly before allocating:
    // percent-encoding triples the text and MIME base64 adds line breaks.
    if !header.to_ascii_lowercase().ends_with(";base64") {
        if percent_decoded_len(payload) > app_lite_core::MAX_IMAGE_BYTES {
            return Err(too_large());
        }
        return Ok(percent_decode_bytes(payload));
    }
    let digits = payload
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        .count();
    if digits / 4 * 3 + (digits % 4).saturating_sub(1) > app_lite_core::MAX_IMAGE_BYTES {
        return Err(too_large());
    }
    let compact: String = payload
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(compact.trim_end_matches('='))
        .map_err(|_| "图片数据无效".to_owned())
}

fn percent_decoded_len(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut length = 0;
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes[index] == b'%'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit);
        index += if escape { 3 } else { 1 };
        length += 1;
    }
    length
}

/// RFC 3986 percent-decoding to raw bytes: `+` stays `+`, and a `%` not
/// followed by two hex digits stays as written.
fn percent_decode_bytes(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |offset: usize| {
            bytes
                .get(index + offset)
                .and_then(|byte| (*byte as char).to_digit(16))
        };
        match (bytes[index], hex(1), hex(2)) {
            (b'%', Some(high), Some(low)) => {
                decoded.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, ..) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    decoded
}

/// A URL path segment as a file name: percent-decoded, lossy where the
/// bytes are not UTF-8.
fn percent_decode(value: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(value)).into_owned()
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(3, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn serve_once(status: &'static str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        });
        format!("http://{address}/assets/%E5%9B%BE.png?size=large")
    }

    #[test]
    fn pasted_images_come_from_data_files_and_the_network_and_fail_with_a_reason() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("local picture.png");
        std::fs::write(&file, png()).unwrap();
        let data = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png())
        );
        let sources = [
            (data, "数据图".to_owned()),
            (
                url::Url::from_file_path(&file).unwrap().to_string(),
                String::new(),
            ),
            (serve_once("200 OK", png()), String::new()),
            (serve_once("404 Not Found", Vec::new()), String::new()),
            ("data:text/plain;base64,aGVsbG8=".to_owned(), String::new()),
            ("javascript:alert(1)".to_owned(), String::new()),
        ];
        let results = fetch_pasted_images(&sources);
        let titles: Vec<_> = results
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .map(|import| import.title.clone())
                    .map_err(Clone::clone)
            })
            .collect();
        assert_eq!(titles[0], Ok("数据图.png".to_owned()));
        assert_eq!(titles[1], Ok("local picture.png".to_owned()));
        assert_eq!(titles[2], Ok("图.png".to_owned()));
        assert_eq!(titles[3], Err("下载图片失败：HTTP 404".to_owned()));
        assert_eq!(titles[4], Err("不是可识别的图片".to_owned()));
        assert_eq!(titles[5], Err("不支持的图片地址".to_owned()));
        assert!(
            results[..3]
                .iter()
                .all(|result| result.as_ref().unwrap().is_image())
        );
    }
}

#[cfg(test)]
mod data_uri_tests {
    use super::*;

    /// A data URI without `;base64` carries bytes percent-encoded, not text:
    /// a PNG's 0x89 signature and other non-UTF-8 bytes must survive.
    #[test]
    fn a_percent_encoded_data_uri_keeps_the_image_bytes() {
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(5, 4)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(
            std::str::from_utf8(&png).is_err(),
            "fixture has non-UTF-8 bytes"
        );
        let encoded: String = png
            .iter()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'+' => {
                    (*byte as char).to_string()
                }
                _ => format!("%{byte:02X}"),
            })
            .collect();
        assert_eq!(
            decode_data_uri(&format!("image/png,{encoded}")).unwrap(),
            png
        );
        let import = fetch_pasted_images(&[(format!("data:image/png,{encoded}"), "图".into())])
            .pop()
            .unwrap()
            .expect("a valid image");
        assert!(import.is_image());
        assert_eq!(decode_data_uri("text/plain,a+b%2").unwrap(), b"a+b%2");
    }
}

#[cfg(test)]
mod data_uri_limit_tests {
    use super::*;
    use app_lite_core::MAX_IMAGE_BYTES;

    fn percent(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("%{byte:02X}")).collect()
    }

    fn base64_lines(bytes: &[u8]) -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        encoded
            .as_bytes()
            .chunks(76)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    /// The limit is on decoded bytes whatever the encoding: percent-encoding
    /// triples the text and MIME base64 adds line breaks, and neither may
    /// push an image at the limit over it.
    #[test]
    fn the_image_size_limit_applies_to_decoded_bytes_in_every_encoding() {
        let encodings: [(&str, fn(&[u8]) -> String); 3] = [
            ("image/png;base64", |bytes| {
                base64::engine::general_purpose::STANDARD.encode(bytes)
            }),
            ("image/png;base64", base64_lines),
            ("image/png", percent),
        ];
        for (header, encode) in encodings {
            let at_limit = vec![0x89_u8; MAX_IMAGE_BYTES];
            assert_eq!(
                decode_data_uri(&format!("{header},{}", encode(&at_limit)))
                    .map(|bytes| bytes.len()),
                Ok(MAX_IMAGE_BYTES),
                "{header}: exactly at the limit"
            );
            let over = vec![0x89_u8; MAX_IMAGE_BYTES + 1];
            assert_eq!(
                decode_data_uri(&format!("{header},{}", encode(&over))),
                Err("图片太大".to_owned()),
                "{header}: one byte over"
            );
        }

        // A real image at the limit, percent-encoded, is stored as an image.
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(5, 4)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png.resize(MAX_IMAGE_BYTES, 0);
        let import =
            fetch_pasted_images(&[(format!("data:image/png,{}", percent(&png)), "大图".into())])
                .pop()
                .unwrap()
                .expect("an image exactly at the limit");
        assert!(import.is_image());
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn png_of(len: usize) -> Vec<u8> {
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png.resize(len.max(png.len()), 0);
        png
    }

    fn data_uri(bytes: &[u8]) -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    /// An image server that reports each request and answers only when
    /// released; an unreleased one holds the connection open.
    fn gated_server(body: Vec<u8>, requested: mpsc::Sender<()>) -> (String, mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/p.png", listener.local_addr().unwrap());
        let (release, released) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = requested.send(());
            if released.recv().is_err() {
                return;
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        });
        (url, release)
    }

    fn collect(
        sources: Vec<(String, String)>,
        limits: PasteDownloadLimits,
    ) -> (
        PasteDownload,
        mpsc::Receiver<(usize, Result<ResourceImport, String>)>,
    ) {
        let (sender, results) = mpsc::channel();
        let sender = Mutex::new(sender);
        let download = start_pasted_image_downloads(sources, limits, move |index, result| {
            let _ = sender.lock().unwrap().send((index, result));
        });
        (download, results)
    }

    #[test]
    fn a_paste_stages_images_to_files_within_its_total_budget() {
        let limits = PasteDownloadLimits {
            image_bytes: 10 * 1024,
            total_bytes: 25 * 1024,
            workers: 1,
            ..Default::default()
        };
        let sources: Vec<_> = (0..5)
            .map(|_| (data_uri(&png_of(8 * 1024)), String::new()))
            .collect();
        let (download, results) = collect(sources, limits);
        download.wait();
        let mut results: Vec<_> = results.try_iter().collect();
        results.sort_by_key(|(index, _)| *index);
        let outcomes: Vec<_> = results
            .iter()
            .map(|(_, result)| result.as_ref().err().cloned())
            .collect();
        assert_eq!(
            outcomes,
            [
                None,
                None,
                None,
                Some("这次粘贴的图片总量超过上限".to_owned()),
                Some("这次粘贴的图片总量超过上限".to_owned()),
            ]
        );
        for (_, result) in results.into_iter().take(3) {
            let (source, ..) = result.unwrap().into_parts();
            assert!(
                matches!(
                    source,
                    crate::native_editor::images::ResourceSource::File { .. }
                ),
                "staged on disk, not held in memory"
            );
        }
        let (download, results) =
            collect(vec![(data_uri(&png_of(11 * 1024)), String::new())], limits);
        download.wait();
        assert_eq!(results.recv().unwrap().1.err(), Some("图片太大".to_owned()));
    }

    #[test]
    fn stalled_servers_share_one_deadline_and_run_in_parallel() {
        let (requested, arrivals) = mpsc::channel();
        let servers: Vec<_> = (0..4)
            .map(|_| gated_server(png_of(0), requested.clone()))
            .collect();
        let limits = PasteDownloadLimits {
            deadline: Duration::from_millis(1500),
            workers: 4,
            ..Default::default()
        };
        let started = Instant::now();
        let (download, results) = collect(
            servers
                .iter()
                .map(|(url, _)| (url.clone(), String::new()))
                .collect(),
            limits,
        );
        // All four requests are in flight together, not one after another.
        for _ in 0..4 {
            arrivals
                .recv_timeout(Duration::from_secs(10))
                .expect("every request is sent while the others are pending");
        }
        download.wait();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "one deadline for the paste: {:?}",
            started.elapsed()
        );
        let failures: Vec<_> = results.try_iter().map(|(_, result)| result.err()).collect();
        assert_eq!(failures, vec![Some("下载图片超时".to_owned()); 4]);
    }

    #[test]
    fn results_arrive_one_by_one_and_a_cancelled_paste_stops() {
        let (requested, arrivals) = mpsc::channel();
        let (url, release) = gated_server(png_of(0), requested);
        let (download, results) = collect(
            vec![(url, String::new()), (data_uri(&png_of(0)), "即时".into())],
            PasteDownloadLimits {
                workers: 2,
                ..Default::default()
            },
        );
        let (index, ready) = results.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(
            index, 1,
            "the ready image does not wait for the stalled one"
        );
        assert!(ready.unwrap().is_image());
        arrivals.recv_timeout(Duration::from_secs(10)).unwrap();
        download.cancel();
        release.send(()).unwrap();
        download.wait();
        let (index, result) = results.recv().unwrap();
        assert_eq!((index, result.err()), (0, Some("已取消".to_owned())));
    }

    /// A server that takes the request and then says nothing, for longer
    /// than the paste's own deadline: cancelling (or just dropping the
    /// handle, as closing the note does) abandons it at once, frees the
    /// network slot and removes the staging files.
    #[test]
    fn cancelling_abandons_a_silent_server_and_frees_its_slot_and_files() {
        for drop_handle in [false, true] {
            let (requested, arrivals) = mpsc::channel();
            let (url, _never_released) = gated_server(png_of(0), requested);
            let slots = Arc::new(tokio::sync::Semaphore::new(2));
            let (sender, results) = mpsc::channel();
            let sender = Mutex::new(sender);
            let download = start_with_slots(
                vec![(url, String::new())],
                PasteDownloadLimits::default(),
                Arc::clone(&slots),
                move |index, result| {
                    let _ = sender.lock().unwrap().send((index, result.err()));
                },
            );
            arrivals
                .recv_timeout(Duration::from_secs(10))
                .expect("the request reaches the silent server");
            assert_eq!(slots.available_permits(), 1, "one slot in use");
            let staging = download.staging_path().unwrap().to_owned();
            assert!(staging.exists());
            let finished = Arc::clone(&download.finished);
            let started = Instant::now();
            if drop_handle {
                drop(download);
            } else {
                download.cancel();
            }
            assert_eq!(
                results
                    .recv_timeout(Duration::from_secs(5))
                    .expect("delivered promptly"),
                (0, Some("已取消".to_owned())),
                "drop_handle = {drop_handle}"
            );
            let (done, changed) = &*finished;
            let (done, timeout) = changed
                .wait_timeout_while(done.lock().unwrap(), Duration::from_secs(5), |done| !*done)
                .unwrap();
            assert!(*done && !timeout.timed_out(), "the download thread ends");
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(slots.available_permits(), 2, "the slot is free again");
            assert!(!staging.exists(), "staging files are removed");
        }
    }
}
