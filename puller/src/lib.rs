//! This library implements high-level image puller on top of
//! `dkregistry` crate. See [`Puller`](Puller) for more.
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use tracing::{error, instrument, trace};
/// Main type of library, which supports loading and unpacking
/// images.
pub struct Puller {
    /// These secrets will be used to authenticate in registry.
    secrets: Vec<ImagePullSecret>,
}

#[derive(Debug)]
pub enum Tls {
    /// Require TLS
    Enable,
    /// Disable TLS (insecure).
    Disable,
}

impl Default for Tls {
    fn default() -> Self {
        Tls::Enable
    }
}

#[derive(Debug, Default)]
pub struct PullSettings {
    /// Tls mode
    pub tls: Tls,
}

impl Puller {
    /// Creates new Puller.
    pub async fn new() -> Puller {
        Puller {
            secrets: Vec::new(),
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
        pull_settings: PullSettings,
        cancel: CancellationToken,
    ) -> Result<dkregistry::v2::manifest::Manifest, Error> {
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
        if let Tls::Disable = pull_settings.tls {
            config = config.insecure_registry(true);
        }

        let mut client = config.build()?;
        if !client.is_auth().await? {
            let token_scope = format!("repository:{}:pull", image_ref.repository());

            client = client.authenticate(&[&token_scope]).await?;
            if !client.is_auth().await? {
                return Err(Error::LoginFailed);
            }
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
        let destination = destination.to_path_buf();
        tokio::fs::create_dir_all(&destination)
            .await
            .map_err(Error::CreateDest)?;

        trace!(
            image = image_ref.to_raw_string().as_str(),
            destination = %destination.display(),
            "fetching manifest for {}",
            IMAGE_ARCHITECTURE
        );
        let manifest = client
            .get_manifest(&image_ref.repository(), &image_ref.version())
            .await?;

        trace!(manifest = ?manifest, "fetched manifest");

        Self::fetch_layers(client, image_ref, cancel, destination, &manifest).await?;
        Ok(manifest)
    }

    /// This function is called by [`Puller::pull`](Puller::pull) when
    /// client is successfully created.
    #[instrument(skip(client, cancel, destination, manifest))]
    async fn fetch_layers(
        client: dkregistry::v2::Client,
        image_ref: dkregistry::reference::Reference,
        cancel: CancellationToken,
        destination: PathBuf,
        manifest: &dkregistry::v2::manifest::Manifest,
    ) -> Result<(), Error> {
        let digests = manifest.layers_digests(Some(IMAGE_ARCHITECTURE))?;
        let digests_count = digests.len();
        trace!("will fetch {} layers", digests_count);
        for layer_digest in digests.into_iter() {
            Self::fetch_layer(
                client.clone(),
                image_ref.repository(),
                layer_digest,
                cancel.clone(),
                destination.clone(),
            )
            .await?;
        }

        Ok(())
    }

    /// Tries to download and unpack one layer
    #[instrument(skip(client, repo, cancel, destination))]
    async fn fetch_layer(
        client: dkregistry::v2::Client,
        repo: String,
        layer_digest: String,
        cancel: CancellationToken,
        destination: PathBuf,
    ) -> Result<(), Error> {
        trace!(layer_digest = layer_digest.as_str(), "download started");
        let data = client.get_blob(&repo, &layer_digest).await?;
        trace!(size = data.len(), "download succeeded");
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // start separate task which unpacks received layer
        let current_span = tracing::Span::current();
        tokio::task::spawn_blocking(move || {
            let _enter = current_span.enter();
            trace!("Unpacking started");
            let res = dkregistry::render::unpack(&[data], &destination);
            trace!("Unpacking finished");
            res
        })
        .await
        .unwrap()?;

        Ok(())
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
    #[error("failed to create destination dir")]
    CreateDest(#[source] std::io::Error),
}
