use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    time::Duration,
};
use tower::{make::Shared, Layer, Service, ServiceBuilder};

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::Poll,
};

// Logger is a service that wrappers another service
#[derive(Clone)]
pub struct Logger<S> {
    request_total: Arc<AtomicU64>,
    source: String,
    inner: S,
}

impl<S> Logger<S> {
    fn new(inner: S, source: String) -> Self {
        Self {
            request_total: Arc::new(AtomicU64::new(0)),
            source,
            inner,
        }
    }
}

// `S` is the inner service
// `R` is the request
// Both must be Send and 'static because the future might be moved (Send) to a different thread
// that the data must outlive ('static).
impl<S, R> Service<R> for Logger<S>
where
    S: Service<R> + Clone + Send + 'static,
    // Writing to the request can return std::io::Error
    S::Error: From<std::io::Error>,
    S::Future: Send + 'static,
    // We want to write to the request
    R: AsyncWrite + Unpin + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: R) -> Self::Future {
        let inc = self.request_total.fetch_add(1, Ordering::SeqCst);
        let mut service = self.inner.clone();
        let source = self.source.clone();

        let fut = async move {
            req.write_all(format!("logger called: {} times from {}\n", inc, source).as_bytes())
                .await?;

            service.call(req).await
        };

        Box::pin(fut)
    }
}

// Implement layer for Logger service
pub struct LoggerLayer;

impl<S> Layer<S> for LoggerLayer {
    type Service = Logger<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Logger::new(inner, "LoggerLayer".to_string())
    }
}

// Waiter is a service that wrappers another service and waits a certain amount of time
#[derive(Clone)]
struct Waiter<S> {
    duration: Duration,
    inner: S,
}

impl<S> Waiter<S> {
    fn new(inner: S, duration: Duration) -> Self {
        Self { duration, inner }
    }
}

impl<S, R> Service<R> for Waiter<S>
where
    S: Service<R> + Clone + Send + 'static,
    S::Error: From<std::io::Error>,
    S::Future: Send + 'static,
    R: AsyncWrite + Unpin + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: R) -> Self::Future {
        let duration = self.duration.clone();
        let mut service = self.inner.clone();

        let fut = async move {
            req.write_all(format!("waiter waiting: {} seconds\n", duration.as_secs()).as_bytes())
                .await?;

            tokio::time::sleep(duration).await;
            service.call(req).await
        };

        Box::pin(fut)
    }
}

// Final service
#[derive(Clone)]
struct Responder {
    request_total: Arc<AtomicU64>,
}

impl Responder {
    fn new() -> Self {
        Self {
            request_total: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<R> Service<R> for Responder
where
    R: AsyncWrite + Unpin + Send + 'static,
{
    type Response = ();
    type Error = anyhow::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut req: R) -> Self::Future {
        let inc = self.request_total.fetch_add(1, Ordering::SeqCst);
        let fut = async move {
            req.write_all(format!("responder called: {} times\n", inc).as_bytes())
                .await?;

            Ok(())
        };

        Box::pin(fut)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:3000").await?;

    // Create a service from a series of layers/service
    let svc = ServiceBuilder::new()
        .layer_fn(|service| Logger::new(service, "layer_fn".to_string()))
        .layer(LoggerLayer)
        .layer_fn(|service| Waiter::new(service, Duration::from_secs(2)))
        .service(Responder::new());

    // A factory for creating services from the ServiceBuilder service
    let mut factory_svc = Shared::new(svc);

    loop {
        let (stream, _) = listener.accept().await?;

        // Create a Logger<Logger<Responder>> service
        let mut svc = factory_svc.call(()).await?;

        tokio::spawn(svc.call(stream));
    }
}
