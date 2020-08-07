//! This library implements high-level image puller on top of
//! `dkregistry` crate. See [`Puller`](Puller) for more.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Semaphore;
use tracing::{error, instrument, trace, warn};
use tracing_futures::Instrument;
/// Main type of library, which supports loading and unpacking
/// images.
pub struct Puller {
    /// These secrets will be used to authenticate in registry.
    secrets: Vec<ImagePullSecret>,
    /// Used to limit parallel downloads
    download_semaphore: Arc<Semaphore>,
}

const DEFAULT_DOWNLOADS_LIMIT: usize = 3;

impl Puller {
    /// Creates new Puller.
    pub async fn new() -> Puller {
        Puller {
            secrets: Vec::new(),
            download_semaphore: Arc::new(Semaphore::new(DEFAULT_DOWNLOADS_LIMIT)),
        }
    }

    pub fn set_secrets(&mut self, secrets: Vec<ImagePullSecret>) -> &mut Self {
        self.secrets = secrets;
        self
    }

    /// Starts pulling an image.
    ///
    /// `image` is image reference in usual format, e.g. `alpine`,
    /// `foo/bar`, `cr.com/repo1/repo2/repo3/superimage`.
    ///
    /// `destination` is filesystem path image should be unpacked to.
    ///
    /// `cancel` is CancellationToken that can be used to cancel
    /// pending operation on best effort basis.
    ///
    /// On success returns channel that will receive final outcome when operation
    /// completed.
    #[instrument(skip(self, cancel))]
    pub async fn pull(
        &self,
        image: &str,
        destination: &Path,
        cancel: tokio::sync::CancellationToken,
    ) -> Result<((), dkregistry::v2::manifest::Manifest), Error> {
        let image_ref: dkregistry::reference::Reference = image.parse()?;
        trace!(
            registry = image_ref.registry().as_str(),
            repository = image_ref.repository().as_str(),
            image = image_ref.version().as_str()
        );
        let mut config = dkregistry::v2::Config::default();
        let creds = self.find_credentials(&image_ref.registry());
        config = config
            .username(creds.0)
            .password(creds.1)
            .registry(&image_ref.registry());
        let mut client = config.build()?;
        if !client.is_auth(None).await? {
            let token_scope = format!("repository:{}:pull", image_ref.repository());

            let token = client.login(&[&token_scope]).await?;
            if !client.is_auth(Some(token.token())).await? {
                return Err(Error::LoginFailed);
            }
            client.set_token(Some(token.token()));
        }
        // this check is used to report missing image nicier
        {
            let types = client
                .has_manifest(&image_ref.repository(), &image_ref.version(), None)
                .await?;
            if types.is_none() {
                return Err(Error::ImageNotExists(image.to_string()));
            }
        }
        // this task downloads the manifest
        let sem = self.download_semaphore.clone();
        let destination = destination.to_path_buf();

        trace!(
            image = image_ref.to_raw_string().as_str(),
            destination = %destination.display(),
            "fetching manifest for {}",
            IMAGE_ARCHITECTURE
        );
        let manifest = client
            .get_manifest(&image_ref.repository(), &image_ref.version())
            .await?;

        let res = Self::fetch_layers(client, image_ref, cancel, sem, destination, &manifest).await;
        Ok((res.unwrap(), manifest))
    }

    /// This function is called by [`Puller::pull`](Puller::pull) when
    /// client is successfully created.
    #[instrument(skip(client, download_semaphore, cancel, destination))]
    async fn fetch_layers(
        client: dkregistry::v2::Client,
        image_ref: dkregistry::reference::Reference,
        cancel: tokio::sync::CancellationToken,
        download_semaphore: Arc<Semaphore>,
        destination: PathBuf,
        manifest: &dkregistry::v2::manifest::Manifest
    ) -> Result<(), Error> {
        let digests = manifest.layers_digests(Some(IMAGE_ARCHITECTURE))?;
        let digests_count = digests.len();
        trace!("will fetch {} layers", digests_count);
        let (tx, mut rx) = tokio::sync::mpsc::channel(digests_count);
        for (layer_id, layer_digest) in digests.into_iter().enumerate() {
            let client = client.clone();
            let sem = download_semaphore.clone();
            let repo = image_ref.repository();
            let tx = tx.clone();
            let cancel = cancel.clone();
            tokio::task::spawn(
                Self::fetch_layer(client, sem, repo, tx, cancel, layer_digest, layer_id)
                    .in_current_span(),
            );
        }
        drop(tx);

        let mut layers = Vec::new();
        layers.resize_with(digests_count, Vec::new);
        let mut received_count = 0;
        while let Some((layer_id, layer_data)) = rx.recv().await {
            layers[layer_id] = layer_data?;
            received_count += 1;
        }

        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // operation can not be cancelled arter this point

        assert_eq!(received_count, digests_count);
        trace!("Starting unpacking");

        // start separate task which unpacks received layers
        let current_span = tracing::Span::current();
        tokio::task::spawn_blocking(move || {
            let _enter = current_span.enter();
            trace!("Unpacking started");
            let res = dkregistry::render::unpack(&layers, &destination);
            trace!("Unpacking finished");
            res
        })
        .await
        .unwrap()?;
        if cancel.is_cancelled() {
            warn!("cancellation request was ignored");
        }

        Ok(())
    }

    /// Tries to download one layer
    #[instrument(skip(client, sem, tx, cancel, repo, layer_digest))]
    async fn fetch_layer(
        client: dkregistry::v2::Client,
        sem: Arc<Semaphore>,
        repo: String,
        mut tx: tokio::sync::mpsc::Sender<(usize, Result<Vec<u8>, dkregistry::errors::Error>)>,
        cancel: tokio::sync::CancellationToken,
        layer_digest: String,
        layer_id: usize,
    ) {
        let _can_download = sem.acquire_owned().await;
        trace!(layer_digest = layer_digest.as_str(), "download started");
        tokio::select! {
            get_layer_result = client.get_blob(&repo, &layer_digest) => {
                match &get_layer_result {
                    Ok(vec) => {
                        trace!(size=vec.len(), "download succeeded");
                    }
                    Err(err) => {
                        error!(error=%err, "layer failed to download");
                    }
                }
                tx.send((layer_id, get_layer_result)).await.expect("should never panic");
            }
            _ = cancel.cancelled() => {
                warn!("layer download was cancelled");
                // do nothing, the whole pull is cancelled
            }
        }
    }

    /// Tries to lookup credentials for `registry`
    #[instrument(skip(self))]
    fn find_credentials(&self, registry: &str) -> (Option<String>, Option<String>) {
        for cred in &self.secrets {
            if cred.registry == registry {
                trace!(credentials=%cred, "found credentials");
                return (cred.username.clone(), cred.password.clone());
            }
        }
        trace!("no credentials found");
        (None, None)
    }
}

// TODO support other architectures
const IMAGE_ARCHITECTURE: &str = "amd64";

// debug missing because it can accidentally reveal sensitive data
pub struct ImagePullSecret {
    pub registry: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ImagePullSecret {
    /// Tries to find `ImagePullSecret`s in parsed docker config.
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let data = std::fs::read("~/.docker/config.json")?;
    /// let data = serde_json::from_slice(&data)?;
    /// let secrets = puller::ImagePullSecret::parse_docker_config(&data);
    /// for sec in secrets.unwrap_or_default() {
    ///     println!("{}", sec);
    /// }  
    /// # Ok(())
    /// # }
    /// ```
    pub fn parse_docker_config(docker_config: &serde_json::Value) -> Option<Vec<ImagePullSecret>> {
        let docker_config = docker_config.as_object()?;
        let auths = docker_config.get("auths")?.as_object()?;
        let mut secrets = Vec::new();
        for (registry, auth_data) in auths {
            if let Some(sec) = Self::parse_from_docker_config_auth_entry(registry, auth_data) {
                secrets.push(sec);
            }
        }
        Some(secrets)
    }

    /// Tries to find `ImagePullSecret`s in Kubernetes Secret.
    #[cfg(feature = "k8s")]
    pub fn parse_kubernetes_secret(
        secret: &k8s_openapi::api::core::v1::Secret,
    ) -> Option<Vec<ImagePullSecret>> {
        let data = secret.string_data.as_ref()?;
        let dockerconfig = data.get(".dockerconfigjson")?;
        let dockerconfig = base64::decode(dockerconfig).ok()?;
        let dockerconfig = serde_json::from_slice(&dockerconfig).ok()?;
        Self::parse_docker_config(&dockerconfig)
    }

    fn parse_from_docker_config_auth_entry(
        registry: &str,
        auth_data: &serde_json::Value,
    ) -> Option<ImagePullSecret> {
        let auth = auth_data.as_object()?.get("auth")?.as_str()?;
        let auth = base64::decode(auth).ok()?;
        let auth = String::from_utf8(auth).ok()?;

        let mut login_and_password = auth.splitn(2, ':');
        let login = login_and_password.next()?;
        let password = login_and_password.next()?;
        Some(ImagePullSecret {
            registry: registry.to_string(),
            username: Some(login.to_string()),
            password: Some(password.to_string()),
        })
    }
}

impl std::fmt::Display for ImagePullSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let username = match &self.username {
            Some(u) => u.as_str(),
            None => "<missing>",
        };
        let password = match &self.password {
            Some(_) => "<provided>",
            None => "<missing>",
        };
        write!(
            f,
            "registry: {}; username: {}, password: {}",
            &self.registry, username, password
        )
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("registry communication error")]
    Client(#[from] dkregistry::errors::Error),
    #[error("invalid reference")]
    RefParse(#[from] dkregistry::reference::ReferenceParseError),
    #[error("login failure: obtained token is invalid")]
    LoginFailed,
    #[error("image {0} does not exist")]
    ImageNotExists(String),
    #[error("unpack failed")]
    Unpack(#[from] dkregistry::render::RenderError),
    #[error("operation was cancelled")]
    Cancelled,
}
