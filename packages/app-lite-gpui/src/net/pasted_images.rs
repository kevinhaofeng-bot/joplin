//! Bytes for images inside HTML pasted from another app. Evernote keeps such
//! images as note resources rather than hot-linking them; so do we.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use gpui::ImageFormat;

use crate::native_editor::images::{ImagePayload, ResourceImport};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";
/// One paste may carry a whole article; images past this stay as links.
pub(crate) const MAX_PASTED_IMAGES: usize = 200;

pub(crate) fn fetch_pasted_images(
    sources: &[(String, String)],
) -> Vec<Result<ResourceImport, String>> {
    let client = super::default_image_request_headers(USER_AGENT)
        .ok()
        .and_then(|headers| {
            reqwest::blocking::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::limited(10))
                .default_headers(headers)
                .build()
                .ok()
        });
    sources
        .iter()
        .enumerate()
        .map(|(index, (source, alt))| {
            if index >= MAX_PASTED_IMAGES {
                return Err(format!("一次最多粘贴 {MAX_PASTED_IMAGES} 张图片"));
            }
            fetch_pasted_image(client.as_ref(), source, alt)
        })
        .collect()
}

fn fetch_pasted_image(
    client: Option<&reqwest::blocking::Client>,
    source: &str,
    alt: &str,
) -> Result<ResourceImport, String> {
    let (bytes, name) = if let Some(data) = strip_ascii_prefix(source, "data:") {
        (decode_data_uri(data)?, None)
    } else {
        let url = url::Url::parse(source).map_err(|_| "图片地址无效".to_owned())?;
        let name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(percent_decode)
            .filter(|name| name.contains('.') && !name.starts_with('.'));
        let bytes = match url.scheme() {
            "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| "图片文件路径无效".to_owned())?;
                read_bounded_file(&path)?
            }
            "http" | "https" => download(
                client.ok_or_else(|| "无法建立网络连接".to_owned())?,
                url.as_str(),
            )?,
            _ => return Err("不支持的图片地址".to_owned()),
        };
        (bytes, name)
    };
    let format = image::guess_format(&bytes)
        .ok()
        .and_then(gpui_format)
        .ok_or_else(|| "不是可识别的图片".to_owned())?;
    let mut payload = ImagePayload::new(format, bytes);
    payload.name = name.or_else(|| {
        let alt = alt.trim();
        (!alt.is_empty() && alt.chars().count() <= 120).then(|| alt.to_owned())
    });
    ResourceImport::from_image_payload(payload).map_err(|error| error.to_string())
}

fn download(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().map_err(|error| {
        if error.is_timeout() {
            "下载图片超时".to_owned()
        } else {
            format!("下载图片失败：{error}")
        }
    })?;
    if !response.status().is_success() {
        return Err(format!("下载图片失败：HTTP {}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > app_lite_core::MAX_IMAGE_BYTES as u64)
    {
        return Err("图片太大".to_owned());
    }
    read_bounded(response)
}

fn read_bounded_file(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("无法读取图片文件：{error}"))?;
    read_bounded(file)
}

fn read_bounded(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(app_lite_core::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取图片失败：{error}"))?;
    if bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
        return Err("图片太大".to_owned());
    }
    Ok(bytes)
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

fn gpui_format(format: image::ImageFormat) -> Option<ImageFormat> {
    Some(match format {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::WebP => ImageFormat::Webp,
        image::ImageFormat::Bmp => ImageFormat::Bmp,
        image::ImageFormat::Tiff => ImageFormat::Tiff,
        _ => return None,
    })
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
        assert_eq!(titles[0], Ok("数据图".to_owned()));
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
