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
    // Base64 is 4/3 of the decoded size; reject before allocating.
    if payload.len() / 4 * 3 > app_lite_core::MAX_IMAGE_BYTES + 3 {
        return Err("图片太大".to_owned());
    }
    if !header.to_ascii_lowercase().ends_with(";base64") {
        return Ok(percent_decode(payload).into_bytes());
    }
    let compact: String = payload
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(compact.trim_end_matches('='))
        .map_err(|_| "图片数据无效".to_owned())?;
    if bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
        return Err("图片太大".to_owned());
    }
    Ok(bytes)
}

fn percent_decode(value: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", value.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
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
