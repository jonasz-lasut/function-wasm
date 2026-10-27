//! A local TCP listener bridged onto a pod port through the API server's
//! port-forward, so ordinary HTTP clients (the suite's registry pusher, the
//! metrics scrape) reach a pod with no NodePort, ingress or kubectl process.
//! Each accepted connection opens one port-forward stream.

use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, ListParams};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub struct Bridge {
    pub addr: SocketAddr,
    accept: JoinHandle<()>,
}

impl Bridge {
    /// Bridges 127.0.0.1:<random> onto pod:port in namespace.
    pub async fn to_pod(client: Client, namespace: &str, pod: &str, port: u16) -> Result<Bridge> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let pods: Api<Pod> = Api::namespaced(client, namespace);
        let pod = pod.to_string();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let pods = pods.clone();
                let pod = pod.clone();
                tokio::spawn(async move {
                    match pods.portforward(&pod, &[port]).await {
                        Ok(mut forwarder) => {
                            if let Some(mut stream) = forwarder.take_stream(port) {
                                let _ =
                                    tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                                drop(stream);
                            }
                            let _ = forwarder.join().await;
                        }
                        Err(e) => eprintln!("port-forward to {pod}:{port}: {e}"),
                    }
                });
            }
        });
        Ok(Bridge { addr, accept })
    }

    /// Bridges onto the first Running pod a label selector finds.
    pub async fn to_labelled(
        client: Client,
        namespace: &str,
        selector: &str,
        port: u16,
    ) -> Result<Bridge> {
        let pod = running_pod(&client, namespace, selector).await?;
        Self::to_pod(client, namespace, &pod, port).await
    }

    pub fn url(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://{}{path}", self.addr)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// The name of a Running pod matching selector; an error names the
/// selector when there is none.
pub async fn running_pod(client: &Client, namespace: &str, selector: &str) -> Result<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let list = pods
        .list(&ListParams::default().labels(selector))
        .await
        .with_context(|| format!("listing pods {selector} in {namespace}"))?;
    list.items
        .iter()
        .find(|p| {
            p.status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .is_some_and(|phase| phase == "Running")
                && p.metadata.deletion_timestamp.is_none()
        })
        .and_then(|p| p.metadata.name.clone())
        .with_context(|| format!("no running pod {selector} in {namespace}"))
}
