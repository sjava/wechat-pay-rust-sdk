use crate::debug;
use crate::error::PayError;
use crate::model::AppParams;
use crate::model::H5Params;
use crate::model::JsapiParams;
use crate::model::MicroParams;
use crate::model::NativeParams;
use crate::model::ParamsTrait;
use crate::pay::{WechatPay, WechatPayTrait};
use crate::request::HttpMethod;
use crate::response::AppResponse;
use crate::response::H5Response;
use crate::response::JsapiResponse;
use crate::response::MicroResponse;
use crate::response::ResponseTrait;
use crate::response::{CertificateResponse, NativeResponse};
use reqwest::header::CONTENT_TYPE;
use reqwest::header::{HeaderMap, REFERER};
use reqwest::multipart::{Form, Part};
use rsa::sha2::{Digest, Sha256};
use serde_json::json;
use serde_json::{Map, Value};

use std::collections::HashSet;
use std::sync::OnceLock;

static SUPPORTED_EXTENSIONS: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn is_supported_image(extension: &str) -> bool {
    let extensions = SUPPORTED_EXTENSIONS
        .get_or_init(|| vec!["jpg", "jpeg", "png", "bmp"].into_iter().collect());
    extensions.contains(extension.to_lowercase().as_str())
}
impl WechatPay {
    pub(crate) async fn pay<P: ParamsTrait, R: ResponseTrait>(
        &self,
        method: HttpMethod,
        url: &str,
        json: P,
    ) -> Result<R, PayError> {
        let json_str = json.to_json();
        debug!("json_str: {}", json_str);
        let mut map: Map<String, Value> = serde_json::from_str(&json_str)?;
        map.insert("appid".to_owned(), self.appid().into());
        map.insert("mchid".to_owned(), self.mch_id().into());
        map.insert("notify_url".to_owned(), self.notify_url().into());
        let body = serde_json::to_string(&map)?;
        let headers = self.build_header(method.clone(), url, body.as_str())?;
        let client = reqwest::Client::new();
        let url = format!("{}{}", self.base_url(), url);
        debug!("url: {} body: {}", url, body);
        let builder = match method {
            HttpMethod::GET => client.get(url),
            HttpMethod::POST => client.post(url),
            HttpMethod::PUT => client.put(url),
            HttpMethod::DELETE => client.delete(url),
            HttpMethod::PATCH => client.patch(url),
        };

        builder
            .headers(headers)
            .body(body)
            .send()
            .await?
            .json::<R>()
            .await
            .map(Ok)?
    }

    pub(crate) async fn get_pay<R: ResponseTrait>(&self, url: &str) -> Result<R, PayError> {
        let body = "";
        let headers = self.build_header(HttpMethod::GET, url, body)?;
        let client = reqwest::Client::new();
        let url = format!("{}{}", self.base_url(), url);
        debug!("url: {} body: {}", url, body);
        client
            .get(url)
            .headers(headers)
            .body(body)
            .send()
            .await?
            .json::<R>()
            .await
            .map(Ok)?
    }

    pub async fn h5_pay(&self, params: H5Params) -> Result<H5Response, PayError> {
        let url = "/v3/pay/transactions/h5";
        self.pay(HttpMethod::POST, url, params).await
    }
    pub async fn app_pay(&self, params: AppParams) -> Result<AppResponse, PayError> {
        let url = "/v3/pay/transactions/app";
        self.pay(HttpMethod::POST, url, params)
            .await
            .map(|mut result: AppResponse| {
                if let Some(prepay_id) = &result.prepay_id {
                    result.sign_data = Some(self.mut_sign_data("", prepay_id));
                }
                result
            })
    }
    pub async fn jsapi_pay(&self, params: JsapiParams) -> Result<JsapiResponse, PayError> {
        let url = "/v3/pay/transactions/jsapi";
        self.pay(HttpMethod::POST, url, params)
            .await
            .map(|mut result: JsapiResponse| {
                if let Some(prepay_id) = &result.prepay_id {
                    result.sign_data = Some(self.mut_sign_data("", prepay_id));
                }
                result
            })
    }
    pub async fn micro_pay(&self, params: MicroParams) -> Result<MicroResponse, PayError> {
        let url = "/v3/pay/transactions/jsapi";
        self.pay(HttpMethod::POST, url, params)
            .await
            .map(|mut result: MicroResponse| {
                if let Some(prepay_id) = &result.prepay_id {
                    result.sign_data = Some(self.mut_sign_data("", prepay_id));
                }
                result
            })
    }
    pub async fn native_pay(&self, params: NativeParams) -> Result<NativeResponse, PayError> {
        let url = "/v3/pay/transactions/native";
        self.pay(HttpMethod::POST, url, params).await
    }

    pub async fn certificates(&self) -> Result<CertificateResponse, PayError> {
        let url = "/v3/certificates";
        self.get_pay(url).await
    }
    pub async fn get_weixin<S>(&self, h5_url: S, referer: S) -> Result<Option<String>, PayError>
    where
        S: AsRef<str>,
    {
        let client = reqwest::Client::new();
        let mut headers = HeaderMap::new();
        headers.insert(REFERER, referer.as_ref().parse().unwrap());
        let text = client
            .get(h5_url.as_ref())
            .headers(headers)
            .send()
            .await?
            .text()
            .await?;
        text.split("\n")
            .find(|line| line.contains("weixin://"))
            .map(|line| {
                line.split(r#"""#)
                    .find(|line| line.contains("weixin://"))
                    .map(|line| line.to_string())
            })
            .ok_or_else(|| PayError::WeixinNotFound)
    }
    pub async fn upload_image(
        &self,
        image: Vec<u8>,
        filename: &str,
    ) -> Result<crate::response::UploadResponse, PayError> {
        const MAX_SIZE: usize = 2 * 1024 * 1024;
        const URL: &str = "/v3/merchant/media/upload";
        // image's size must be less than 2M
        if image.len() > MAX_SIZE {
            return Err(PayError::WechatError(format!(
                "Image size ({} bytes) exceeds the maximum allowed size ({} bytes)",
                image.len(),
                MAX_SIZE
            )));
        }
        // check image format is supported
        let ext = filename.split('.').last().ok_or_else(|| {
            PayError::WechatError("Invalid filename, no extension found".to_string())
        })?;
        if !is_supported_image(ext) {
            return Err(PayError::WechatError(format!(
                "Unsupported image format: {}",
                ext
            )));
        }

        // calculate sha256
        let mut hasher = Sha256::new();
        hasher.update(&image);
        let hash = hasher.finalize();
        let hash = hex::encode(hash.as_slice());

        let meta = json!( {
            "filename": filename,
            "sha256": hash
        });

        let method = HttpMethod::POST;
        let mut headers = self.build_header(method.clone(), &URL, meta.to_string())?;
        headers.insert(CONTENT_TYPE, "multipart/form-data".parse().unwrap());

        let mut json_part_headers = HeaderMap::new();
        json_part_headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        let json_part = Part::text(meta.to_string()).headers(json_part_headers);

        let mime = match ext {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "bmp" => "image/bmp",
            _ => "image/jpeg",
        };

        let form_part = Part::bytes(image)
            .file_name(filename.to_string())
            .mime_str(mime)?;

        let form = Form::new().part("meta", json_part).part("file", form_part);

        let client = reqwest::Client::new();
        let url = format!("{}{}", self.base_url(), URL);
        client
            .post(url)
            .headers(headers)
            .multipart(form)
            .send()
            .await?
            .json()
            .await
            .map(Ok)?
    }
}

#[cfg(test)]
mod tests {
    use crate::model::NativeParams;
    use crate::pay::WechatPay;
    use dotenvy::dotenv;
    use tracing::debug;

    #[inline]
    fn init_log() {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_line_number(true)
            .init();
    }

    #[tokio::test]
    pub async fn test_native_pay() {
        init_log();
        dotenv().ok();
        let wechat_pay = WechatPay::from_env();
        let body = wechat_pay
            .native_pay(NativeParams::new("测试支付1分", "1243243", 1.into()))
            .await
            .expect("pay fail");
        debug!("body: {:?}", body);
    }
}
