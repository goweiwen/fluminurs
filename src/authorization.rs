use select::document::Document;
use select::predicate::Attr;
use std::collections::HashMap;
use reqwest::{Client, RedirectPolicy, RequestBuilder, Response};
use reqwest::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use url::Url;
use serde::{Serialize, Deserialize};

const AUTH_BASE_URL: &'static str = "https://luminus.nus.edu.sg";
const DISCOVERY_PATH: &'static str = "/v2/auth/.well-known/openid-configuration";
const CLIENT_ID: &'static str = "verso";
const SCOPE: &'static str = "profile email role openid lms.read calendar.read lms.delete lms.write calendar.write gradebook.write offline_access";
const RESPONSE_TYPE: &'static str = "id_token token code";
const REDIRECT_URI: &'static str = "https://luminus.nus.edu.sg/auth/callback";

#[derive(Deserialize)]
struct Discovery {
    authorization_endpoint: String,
}

#[derive(Deserialize)]
struct Xsrf {
    name: String,
    value: String,
}

impl Xsrf {
    fn build_login_params<'a>(&'a self, username: &'a str, password: &'a str) -> HashMap<&'a str, &'a str> {
        let mut params = HashMap::new();
        params.insert("username", username);
        params.insert("password", password);
        params.insert(&self.name, &self.value);
        params
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginInfo {
    anti_forgery: Xsrf,
    login_url: String,
}

pub struct Authorization {
    pub jwt: Option<String>,
    cookies: HashMap<String, String>,
}

fn full_auth_url(path: &str) -> Url {
    Url::parse(AUTH_BASE_URL).and_then(|u| u.join(path)).expect("Unable to join URL's")
}

pub fn auth_endpoint_uri() -> Url {
    let discovery_url = full_auth_url(DISCOVERY_PATH);
    let discovery: Discovery = reqwest::get(discovery_url).expect("Failed to HTTP GET the discovery path").json().expect("Unable to deserialize discovery json");
    let mut auth_url = Url::parse(&discovery.authorization_endpoint).expect("Unable to parse discovery url");
    add_auth_params(&mut auth_url);
    auth_url
}

fn add_auth_params(auth_url: &mut Url) {
    auth_url.query_pairs_mut()
        .append_pair("state", &generate_random_bytes(16))
        .append_pair("nonce", &generate_random_bytes(16))
        .append_pair("client_id", CLIENT_ID)
        .append_pair("scope", SCOPE)
        .append_pair("response_type", RESPONSE_TYPE)
        .append_pair("redirect_uri", REDIRECT_URI);
}

fn build_client() -> Result<Client, &'static str> {
    Client::builder().redirect(RedirectPolicy::none()).build().map_err(|_|"Unable to create HTTP client")
}

pub fn generate_random_bytes(size: usize) -> String {
    (0..size).map(|_| format!("{:02x}", rand::random::<u8>())).collect()
}

fn get_redirect_url(response: Response) -> Result<Url, &'static str> {
    let location = response.headers().get(LOCATION).ok_or("Invalid response from server, expected redirection")?
        .to_str().map_err(|_| "Unable to read location header")?.to_string();
    let url = Url::parse(&location).map_err(|_| " Unable to parse the url of location")?;
    Ok(url)
}

impl Authorization {
    pub fn new() -> Authorization {
        Authorization { jwt: None, cookies: HashMap::new() }
    }

    fn http_post<T: Serialize + ?Sized>(&mut self, url: Url, query: &T) -> Result<Response, &'static str> {
        let client = build_client()?;
        let response = self.add_cookie_header(client.post(url)).form(query).send().map_err(|_|"Failed HTTP request")?;
        for c in response.headers().get_all(SET_COOKIE).iter() {
            let cookie = c.to_str().map_err(|_| "Unable to read set-cookie header")?.to_string();
            self.add_cookie(cookie);
        }
        Ok(response)
    }

    fn http_get(&mut self, url: Url) -> Result<Response, &'static str> {
        let client = build_client()?;
        let response = self.add_cookie_header(client.get(url)).send().map_err(|_| "Failed HTTP request")?;
        for c in response.headers().get_all(SET_COOKIE).iter() {
            let cookie = c.to_str().map_err(|_| "Unable to read set-cookie header")?.to_string();
            self.add_cookie(cookie);
        }
        Ok(response)
    }

    pub fn login(&mut self, username: &str, password: &str) -> Result<bool, &'static str> {
        let login_info = self.auth_login_info()?;
        let url = full_auth_url(&login_info.login_url);
        let params = login_info.anti_forgery.build_login_params(username, password);
        let first_response = self.http_post(url, &params)?;
        if !first_response.status().is_redirection() {
            return Err("Invalid credentials");
        }
        let second_url = get_redirect_url(first_response)?;
        let callback_url = get_redirect_url(self.http_get(second_url)?)?;
        return self.handle_callback(callback_url);
    }

    pub fn renew(&mut self) -> Result<bool, &'static str> {
        if self.jwt.is_none() {
            return Err("Please login first.")
        }
        let auth_url = auth_endpoint_uri();
        let callback_url = get_redirect_url(self.http_get(auth_url)?)?;
        println!("{}", &callback_url);
        return self.handle_callback(callback_url);
    }

    fn handle_callback(&mut self, callback_url: Url) -> Result<bool, &'static str> {
        let fragment = callback_url.fragment().ok_or("Invalid callback")?;
        let response: HashMap<String, String> = serde_urlencoded::from_str(&fragment).map_err(|_| "Invalid callback")?;
        self.jwt = Some(response["id_token"].to_owned());
        let idsrv = self.cookies["idsrv"].to_owned();
        self.cookies = HashMap::new();
        self.cookies.insert("idsrv".to_string(), idsrv);
        Ok(true)
    }


    fn auth_login_info(&mut self) -> Result<LoginInfo, &'static str> {
        let auth_url = auth_endpoint_uri();
        let second_url = get_redirect_url(self.http_get(auth_url)?)?;
        let second_body = self.http_get(second_url)?.text().map_err(|_| "Unable to read HTTP response body")?;
        let raw_json = Document::from(second_body.as_str()).find(Attr("id", "modelJson")).last().ok_or("No JSON was sent")?.text().trim().to_owned();
        let json = htmlescape::decode_html(&raw_json).map_err(|_| "Unable to decode HTML entities")?;
        let login_info: LoginInfo = serde_json::from_str(&json).map_err(|_| "Unable to decode JSON")?;
        Ok(login_info)
    }

    fn add_cookie(&mut self, set_cookie_header: String) {
        let c = cookie::Cookie::parse(set_cookie_header).expect("Unable to parse cookie");
        let (name, value) = c.name_value();
        self.cookies.insert(name.to_owned(), value.to_owned());
    }

    fn generate_cookie_header(&self) -> String {
        self.cookies.iter().map(|(k, v)| format!("{}={}; ", k, v)).collect()
    }

    fn add_cookie_header(&mut self, request_builder: RequestBuilder) -> RequestBuilder {
        let cookie_value = HeaderValue::from_str(&self.generate_cookie_header()).expect("Unable to add cookie header");
        request_builder.header(COOKIE, cookie_value)
    }
}