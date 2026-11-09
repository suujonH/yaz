//! 文件上传与下载。

use reqwest::header::{ACCEPT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::multipart::{Form as Multipart, Part};
use reqwest::{Body, Method};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::client::{ZulipClient, decode, header_str, header_u64, parse_result};
use zulip_core::model::UploadedFile;

use crate::error::ApiError;

const CHUNK_SIZE: usize = 80 * 1024;

/// 读取 `reader` 并在每个数据块交给 HTTP 层时报告已发送的字节数。
fn progress_body<R>(reader: R, progress: impl Fn(u64) + Send + Sync + 'static) -> Body
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let stream = futures::stream::try_unfold(
        (reader, 0u64, progress),
        |(mut reader, sent, progress)| async move {
            let mut buffer = vec![0u8; CHUNK_SIZE];
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            buffer.truncate(read);
            let sent = sent + read as u64;
            progress(sent);
            Ok(Some((bytes::Bytes::from(buffer), (reader, sent, progress))))
        },
    );
    Body::wrap_stream(stream)
}

impl ZulipClient {
    /// 上传文件，不设超时。`len` 是 `reader` 将产出的字节数；`progress` 报告已发送的字节数。
    /// 返回的地址形如 `/user_uploads/…`。
    pub async fn upload_file<R>(
        &self,
        reader: R,
        len: u64,
        file_name: &str,
        content_type: &str,
        progress: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<UploadedFile, ApiError>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let part = Part::stream_with_length(progress_body(reader, progress), len);
        let root = self
            .post_file("api/v1/user_uploads", part, file_name, content_type)
            .await?;
        decode(root)
    }

    /// 更换自己的头像。组织禁止更换时服务器返回错误。
    pub async fn upload_avatar(
        &self,
        data: Vec<u8>,
        file_name: &str,
        content_type: &str,
    ) -> Result<(), ApiError> {
        self.post_file(
            "api/v1/users/me/avatar",
            Part::bytes(data),
            file_name,
            content_type,
        )
        .await?;
        Ok(())
    }

    async fn post_file(
        &self,
        path: &str,
        part: Part,
        file_name: &str,
        content_type: &str,
    ) -> Result<Value, ApiError> {
        let part = part
            .file_name(file_name.to_owned())
            .mime_str(content_type)
            .map_err(|e| ApiError::InvalidInput(format!("内容类型无效：{content_type}：{e}")))?;
        let builder = self
            .request_builder(&Method::POST, self.url(path)?)
            .multipart(Multipart::new().part("file", part));
        let reply = self.exchange(builder, &format!("POST {path}")).await?;
        parse_result(reply.status, &reply.body)
    }

    /// 下载组织内的文件（带认证），写入 `target`，不设超时。`progress` 报告已下载的字节数与总字节数（未知时为 `None`）。
    pub async fn download<W>(
        &self,
        relative_path: &str,
        target: &mut W,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<(), ApiError>
    where
        W: AsyncWrite + Unpin,
    {
        let operation = format!("GET {relative_path}");
        let builder = self.request_builder(&Method::GET, self.url(relative_path)?);
        let mut response = self.open(builder, &operation).await?;
        let status = response.status();
        if !status.is_success() {
            let code = status.as_u16();
            return Err(ApiError::api(
                code,
                &format!("HTTP_{code}"),
                format!("下载失败：{status}"),
                None,
            ));
        }

        let total = response.content_length();
        let mut done = 0u64;
        while let Some(chunk) = self
            .guarded(async {
                response
                    .chunk()
                    .await
                    .map_err(|e| ApiError::from_reqwest(e, &operation))
            })
            .await?
        {
            target.write_all(&chunk).await?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        target.flush().await?;
        Ok(())
    }

    /// 文件大小。先用 HEAD 读 `Content-Length`；不支持时改用 `Range: bytes=0-0` 从 `Content-Range` 读取总大小；
    /// 都失败时返回 `None`。
    pub async fn head_size(&self, relative_path: &str) -> Result<Option<u64>, ApiError> {
        match self.probe_size(relative_path).await {
            Err(error) if error.is_cancelled() => Err(error),
            Err(_) => Ok(None),
            Ok(size) => Ok(size),
        }
    }

    async fn probe_size(&self, relative_path: &str) -> Result<Option<u64>, ApiError> {
        let url = self.url(relative_path)?;
        // 不要压缩：要的是文件本身的大小
        let identity = |method: Method| {
            self.request_builder(&method, url.clone())
                .header(ACCEPT_ENCODING, "identity")
        };

        let head = self
            .open(identity(Method::HEAD), &format!("HEAD {relative_path}"))
            .await?;
        if head.status().is_success()
            && let Some(length) = header_u64(head.headers(), &CONTENT_LENGTH)
        {
            return Ok(Some(length));
        }

        let ranged = self
            .open(
                identity(Method::GET).header(RANGE, "bytes=0-0"),
                &format!("GET {relative_path}"),
            )
            .await?;
        // Content-Range: bytes 0-0/总大小
        let total = header_str(ranged.headers(), &CONTENT_RANGE)
            .and_then(|range| range.rsplit_once('/'))
            .and_then(|(_, total)| total.trim().parse().ok());
        if total.is_some() {
            return Ok(total);
        }
        if ranged.status() == reqwest::StatusCode::OK {
            return Ok(header_u64(ranged.headers(), &CONTENT_LENGTH));
        }
        Ok(None)
    }
}
