//! Features built into the program on top of the plugin host's built-in plugins (see
//! `zulip_plugin_host::builtin`): the model center and translation. They have their own pages in
//! the settings window and are not in the plugin list.

pub mod model_center;
pub mod translator;

use std::sync::Arc;

use zulip_plugin_host::PluginHost;

/// Adds the built-in plugins to the host (before it loads plugins). The model center comes first:
/// translation depends on it.
pub fn register(host: &PluginHost) {
    let builtins: [(&str, Arc<dyn zulip_plugin_host::BuiltinPlugin>); 2] = [
        (model_center::MANIFEST, Arc::new(model_center::ModelCenter)),
        (
            translator::MANIFEST,
            Arc::new(translator::Translator::default()),
        ),
    ];
    for (manifest, builtin) in builtins {
        if let Err(error) = host.add_builtin(manifest, builtin) {
            log::error!("内置功能的清单无效：{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use zulip_plugin_host::PluginManifest;

    use super::*;

    #[test]
    fn manifests_are_valid() {
        for (manifest, id) in [
            (model_center::MANIFEST, model_center::ID),
            (translator::MANIFEST, translator::ID),
        ] {
            let parsed =
                PluginManifest::parse(manifest, id, 1, &mut |w| panic!("{id}：{w}")).unwrap();
            assert_eq!(parsed.id, id);
        }
    }
}
