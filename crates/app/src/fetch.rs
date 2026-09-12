use std::sync::Arc;
#[cfg(test)]
use std::{
  collections::HashMap,
  sync::{Mutex, MutexGuard, PoisonError},
};

use futures::{AsyncReadExt as _, future::BoxFuture};
use gpui_kit::http_client::{
  AsyncBody, HttpClient, HttpRequestExt as _, Method, RedirectPolicy, Request, Response, Url,
};
use openit_core::resource::{self, DomainFamily, Resolved};

/// Fetches remote resources after the caller has checked their permission family.
pub trait RemoteFetcher: Send + Sync + 'static {
  /// Fetches one remote resource, following only redirects covered by `family`.
  fn fetch(&self, url: Url, family: DomainFamily) -> BoxFuture<'static, Result<Vec<u8>, FetchError>>;
}

/// HTTP-backed implementation of [`RemoteFetcher`].
pub struct HttpFetcher(Arc<dyn HttpClient>);

impl HttpFetcher {
  /// Creates a fetcher backed by `client`.
  pub fn new(client: Arc<dyn HttpClient>) -> Self {
    Self(client)
  }
}

impl RemoteFetcher for HttpFetcher {
  fn fetch(&self, url: Url, family: DomainFamily) -> BoxFuture<'static, Result<Vec<u8>, FetchError>> {
    let client = Arc::clone(&self.0);
    Box::pin(async move { fetch_remote(client, url, family).await })
  }
}

/// Global remote fetcher used by document views.
pub struct Fetcher(pub Arc<dyn RemoteFetcher>);

impl gpui_kit::Global for Fetcher {}

/// Failure while fetching a remote resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
  /// The server returned a non-success HTTP status.
  Status(u16),
  /// A redirect target was outside the initially approved domain family.
  RedirectLeftFamily(String),
  /// The response required more than the permitted redirect hops.
  TooManyRedirects,
  /// The response body exceeded the image byte limit.
  TooLarge,
  /// The HTTP transport or response body could not be read.
  Transport(String),
}

impl std::fmt::Display for FetchError {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Status(status) => write!(formatter, "HTTP status {status}"),
      Self::RedirectLeftFamily(host) => write!(formatter, "redirect left domain family at {host}"),
      Self::TooManyRedirects => formatter.write_str("too many redirects"),
      Self::TooLarge => formatter.write_str("response body is too large"),
      Self::Transport(error) => write!(formatter, "HTTP transport error: {error}"),
    }
  }
}

const MAX_REDIRECT_HOPS: usize = 5;

async fn fetch_remote(
  client: Arc<dyn HttpClient>,
  mut current_url: Url,
  family: DomainFamily,
) -> Result<Vec<u8>, FetchError> {
  let mut redirect_hops = 0;
  loop {
    let request = Request::builder()
      .method(Method::GET)
      .uri(current_url.as_str())
      .follow_redirects(RedirectPolicy::NoFollow)
      .body(AsyncBody::empty())
      .map_err(|error| FetchError::Transport(error.to_string()))?;
    let response = client
      .send(request)
      .await
      .map_err(|error| FetchError::Transport(error.to_string()))?;

    match response.status().as_u16() {
      301 | 302 | 303 | 307 | 308 => {
        if redirect_hops >= MAX_REDIRECT_HOPS {
          return Err(FetchError::TooManyRedirects);
        }
        redirect_hops += 1;
        let Some(location) = response.headers().get("location") else {
          return Err(FetchError::Transport("redirect response missing Location header".to_owned()));
        };
        let location = location.to_str().map_err(|error| FetchError::Transport(error.to_string()))?;
        let next = current_url
          .join(location)
          .map_err(|error| FetchError::Transport(error.to_string()))?;
        let next_text = next.to_string();
        let next_host = next.host_str().map(str::to_owned);
        let resolved = resource::resolve(&next_text, None);
        let Resolved::Remote(next) = resolved else {
          return Err(FetchError::RedirectLeftFamily(next_host.unwrap_or(next_text)));
        };
        let Some(host) = next.host_str() else {
          return Err(FetchError::RedirectLeftFamily(next.to_string()));
        };
        if !family.covers(host) {
          return Err(FetchError::RedirectLeftFamily(host.to_owned()));
        }
        current_url = next;
      },
      200..=299 => return read_body(response).await,
      status => return Err(FetchError::Status(status)),
    }
  }
}

async fn read_body(response: Response<AsyncBody>) -> Result<Vec<u8>, FetchError> {
  let mut body = response.into_body().take(crate::image_decode::MAX_IMAGE_BYTES + 1);
  let mut bytes = Vec::new();
  body
    .read_to_end(&mut bytes)
    .await
    .map_err(|error| FetchError::Transport(error.to_string()))?;
  if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > crate::image_decode::MAX_IMAGE_BYTES {
    return Err(FetchError::TooLarge);
  }
  Ok(bytes)
}

#[cfg(test)]
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// In-memory fetcher for document image tests.
#[cfg(test)]
#[expect(
  clippy::type_complexity,
  reason = "the fake response table mirrors the requested production fetch result"
)]
pub struct FakeFetcher {
  responses: Mutex<HashMap<String, Result<Vec<u8>, FetchError>>>,
  calls: Mutex<Vec<String>>,
}

#[cfg(test)]
impl FakeFetcher {
  pub(crate) fn new(responses: HashMap<String, Result<Vec<u8>, FetchError>>) -> Self {
    Self {
      responses: Mutex::new(responses),
      calls: Mutex::new(Vec::new()),
    }
  }

  pub(crate) fn calls(&self) -> Vec<String> {
    lock(&self.calls).clone()
  }
}

#[cfg(test)]
impl RemoteFetcher for FakeFetcher {
  fn fetch(&self, url: Url, _family: DomainFamily) -> BoxFuture<'static, Result<Vec<u8>, FetchError>> {
    let key = url.to_string();
    lock(&self.calls).push(key.clone());
    let result = lock(&self.responses)
      .get(&key)
      .cloned()
      .unwrap_or_else(|| Err(FetchError::Transport(format!("no fake response for {key}"))));
    Box::pin(async move { result })
  }
}

#[expect(clippy::future_not_send, reason = "GPUI test contexts are single-threaded")]
#[cfg(test)]
mod tests {
  use super::*;
  use crate::image_decode::MAX_IMAGE_BYTES;
  use openit_core::resource::DomainFamily;

  type ResponseFactory = Arc<dyn Fn(Request<AsyncBody>) -> Response<AsyncBody> + Send + Sync>;

  struct TableHttpClient {
    responses: HashMap<String, ResponseFactory>,
    calls: Arc<Mutex<Vec<String>>>,
  }

  impl TableHttpClient {
    fn new(routes: impl IntoIterator<Item = (String, ResponseFactory)>) -> Arc<Self> {
      Arc::new(Self {
        responses: routes.into_iter().collect(),
        calls: Arc::new(Mutex::new(Vec::new())),
      })
    }

    fn calls(&self) -> Vec<String> {
      lock(&self.calls).clone()
    }
  }

  impl HttpClient for TableHttpClient {
    fn user_agent(&self) -> Option<&gpui_kit::http_client::http::HeaderValue> {
      None
    }

    fn proxy(&self) -> Option<&Url> {
      None
    }

    fn send(&self, request: Request<AsyncBody>) -> BoxFuture<'static, gpui_kit::Result<Response<AsyncBody>>> {
      let url = request.uri().to_string();
      lock(&self.calls).push(url.clone());
      let response = self.responses.get(&url).map(|factory| factory(request));
      Box::pin(
        async move { response.ok_or_else(|| std::io::Error::other(format!("unexpected request: {url}")).into()) },
      )
    }
  }

  fn response(status: u16, location: Option<&str>, body: impl Into<Vec<u8>>) -> ResponseFactory {
    let location = location.map(str::to_owned);
    let body = body.into();
    Arc::new(move |_request| {
      let mut builder = Response::builder().status(status);
      if let Some(location) = location.as_deref() {
        builder = builder.header("location", location);
      }
      builder.body(AsyncBody::from(body.clone())).unwrap()
    })
  }

  #[gpui_kit::test]
  async fn follows_redirects_inside_the_family(_cx: &mut gpui_kit::TestAppContext) {
    let start = "https://a.github.com/x";
    let middle = "https://b.github.com/y";
    let client = TableHttpClient::new([
      (start.to_owned(), response(302, Some(middle), Vec::new())),
      (middle.to_owned(), response(200, None, b"ok".to_vec())),
    ]);
    let fetcher = HttpFetcher::new(client.clone());

    let result = fetcher
      .fetch(Url::parse(start).unwrap(), DomainFamily::of_host("a.github.com"))
      .await;

    assert_eq!(result.unwrap(), b"ok");
    assert_eq!(client.calls(), vec![start, middle]);
  }

  #[gpui_kit::test]
  async fn rejects_redirects_outside_the_family(_cx: &mut gpui_kit::TestAppContext) {
    let start = "https://a.github.com/x";
    let evil = "https://evil.test/y";
    let client = TableHttpClient::new([(start.to_owned(), response(302, Some(evil), Vec::new()))]);
    let fetcher = HttpFetcher::new(client.clone());

    let result = fetcher
      .fetch(Url::parse(start).unwrap(), DomainFamily::of_host("a.github.com"))
      .await;

    assert_eq!(result, Err(FetchError::RedirectLeftFamily("evil.test".to_owned())));
    assert_eq!(client.calls(), vec![start]);
  }

  #[gpui_kit::test]
  async fn rejects_redirects_between_private_suffix_tenants(_cx: &mut gpui_kit::TestAppContext) {
    let start = "https://a.foo.appspot.com/x";
    let evil = "https://evil.appspot.com/y";
    let client = TableHttpClient::new([(start.to_owned(), response(302, Some(evil), Vec::new()))]);
    let fetcher = HttpFetcher::new(client.clone());

    let result = fetcher
      .fetch(Url::parse(start).unwrap(), DomainFamily::of_host("a.foo.appspot.com"))
      .await;

    assert_eq!(result, Err(FetchError::RedirectLeftFamily("evil.appspot.com".to_owned())));
    assert_eq!(client.calls(), vec![start]);
  }

  #[gpui_kit::test]
  async fn stops_after_five_redirect_hops(_cx: &mut gpui_kit::TestAppContext) {
    let routes = (0..=5)
      .map(|index| {
        let current = format!("https://a.github.com/{index}");
        let next = format!("https://a.github.com/{}", index + 1);
        (current, response(302, Some(&next), Vec::new()))
      })
      .collect::<Vec<_>>();
    let client = TableHttpClient::new(routes);
    let fetcher = HttpFetcher::new(client.clone());

    let result = fetcher
      .fetch(
        Url::parse("https://a.github.com/0").unwrap(),
        DomainFamily::of_host("a.github.com"),
      )
      .await;

    assert_eq!(result, Err(FetchError::TooManyRedirects));
    assert_eq!(client.calls().len(), 6);
    assert_eq!(client.calls().last(), Some(&"https://a.github.com/5".to_owned()));
  }

  #[gpui_kit::test]
  async fn rejects_bodies_over_the_image_limit(_cx: &mut gpui_kit::TestAppContext) {
    let start = "https://a.github.com/x";
    let body = vec![0; usize::try_from(MAX_IMAGE_BYTES + 1).unwrap()];
    let client = TableHttpClient::new([(start.to_owned(), response(200, None, body))]);
    let fetcher = HttpFetcher::new(client);

    let result = fetcher
      .fetch(Url::parse(start).unwrap(), DomainFamily::of_host("a.github.com"))
      .await;

    assert_eq!(result, Err(FetchError::TooLarge));
  }

  #[gpui_kit::test]
  async fn returns_http_status_errors(_cx: &mut gpui_kit::TestAppContext) {
    let start = "https://a.github.com/x";
    let client = TableHttpClient::new([(start.to_owned(), response(404, None, Vec::new()))]);
    let fetcher = HttpFetcher::new(client);

    let result = fetcher
      .fetch(Url::parse(start).unwrap(), DomainFamily::of_host("a.github.com"))
      .await;

    assert_eq!(result, Err(FetchError::Status(404)));
  }
}
