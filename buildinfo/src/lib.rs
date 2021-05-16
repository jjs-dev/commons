use once_cell::sync::Lazy;
use serde::Serialize;

#[non_exhaustive]
#[derive(Copy, Clone, Debug, Serialize)]
pub struct BuildInfo {
    pub build_date: Option<&'static str>,
    pub git_revision: Option<&'static str>,
    pub version: Option<&'static str>,
}

impl BuildInfo {
    pub fn get() -> Self {
        let v = BuildInfo::do_get();
        if option_env!("JJS_BUILD_INFO_VERIFY_FULL").is_some() {
            assert!(v.build_date.is_some());
            assert!(v.git_revision.is_some());
            assert!(v.version.is_some());
        }
        v
    }

    fn do_get() -> Self {
        BuildInfo {
            build_date: option_env!("JJS_BUILD_INFO_DATE"),
            git_revision: option_env!("JJS_BUILD_INFO_COMMIT"),
            version: option_env!("JJS_BUILD_INFO_VERSION"),
        }
    }

    pub fn wrap_clap<'a>(app: clap::App<'a>) -> clap::App<'a> {
        static STRING_LONG_VERSION: Lazy<String> = Lazy::new(|| format!("{:#?}", BuildInfo::get()));
        static STRING_SHORT_VERSION: Lazy<String> =
            Lazy::new(|| BuildInfo::get().version.unwrap_or("dev").to_string());
        app.version(STRING_SHORT_VERSION.as_str())
            .long_version(STRING_LONG_VERSION.as_str())
    }
}
