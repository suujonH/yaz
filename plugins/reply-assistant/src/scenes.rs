//! Decides the scene and relationship used when analyzing a message: the topic decides the scene
//! (the default scene if none is set), and the sender's identity is used only if it belongs to this
//! scene, otherwise no relationship is specified; a DM uses the other person's identity (and its
//! scene), the default identity when the other person has none, and the default scene with no
//! relationship when there is no default identity either. An identity is written as "scene
//! id/relationship id".

use zulip_plugin_sdk::serde_json::{Value, json};
use zulip_plugin_sdk::{Conversation, Message, str_of};

use crate::catalog::{CATALOG, Relationship, Scene};
use crate::store;

const DEFAULT_SCENE_KEY: &str = "default_scene";
const DEFAULT_IDENTITY_KEY: &str = "default_identity";

/// The scene and relationship used for analysis; `relationship` of `None` means no relationship is
/// specified.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneChoice {
    pub scene: &'static Scene,
    pub relationship: Option<&'static Relationship>,
}

impl SceneChoice {
    /// For example "Work / Colleague" or "Work (no relationship specified)" (in Chinese).
    pub fn label(&self) -> String {
        match self.relationship {
            Some(relationship) => format!("{} / {}", self.scene.label, relationship.label),
            None => format!("{}（不指定关系）", self.scene.label),
        }
    }

    /// The relationship description handed to the model.
    pub fn note(&self) -> &'static str {
        self.relationship
            .map_or(&self.scene.unspecified_note, |r| &r.note)
    }
}

pub fn resolve(message: &Message, settings: &Value) -> SceneChoice {
    let identity_of = |user_id: i64| {
        store::get_identity(message.account_id, user_id).and_then(|i| parse_identity(&i))
    };
    match &message.conv {
        Conversation::Channel {
            stream_id, topic, ..
        } => {
            let scene = store::get_topic_scene(message.account_id, *stream_id, topic)
                .and_then(|id| CATALOG.scene(&id))
                .unwrap_or_else(|| default_scene(settings));
            let sender = if message.is_outgoing() {
                None
            } else {
                identity_of(message.sender_id)
            };
            SceneChoice {
                scene,
                relationship: sender
                    .filter(|s| std::ptr::eq(s.scene, scene))
                    .and_then(|s| s.relationship),
            }
        }
        Conversation::Direct { user_ids } => {
            // For a one-to-one DM it is the other person; for a group DM it is the sender (a
            // message sent by self has no other person).
            let other = match user_ids.as_slice() {
                [id] => Some(*id),
                _ if message.is_outgoing() => None,
                _ => Some(message.sender_id),
            };
            other
                .and_then(identity_of)
                .or_else(|| parse_identity(str_of(settings, DEFAULT_IDENTITY_KEY)))
                .unwrap_or(SceneChoice {
                    scene: default_scene(settings),
                    relationship: None,
                })
        }
    }
}

pub fn default_scene(settings: &Value) -> &'static Scene {
    CATALOG
        .scene(str_of(settings, DEFAULT_SCENE_KEY))
        .unwrap_or(&CATALOG.scenes[0])
}

/// Parses "scene id/relationship id"; `None` if it does not exist.
pub fn parse_identity(identity: &str) -> Option<SceneChoice> {
    let (scene, relationship) = identity.split_once('/')?;
    let scene = CATALOG.scene(scene)?;
    let relationship = scene.relationships.iter().find(|r| r.id == relationship)?;
    Some(SceneChoice {
        scene,
        relationship: Some(relationship),
    })
}

/// Scene options for the topic panel: the first is to use the default scene (empty value).
pub fn scene_options(settings: &Value) -> Value {
    let default = format!("默认（{}）", default_scene(settings).label);
    let mut options = vec![option("", &default)];
    options.extend(CATALOG.scenes.iter().map(|s| option(&s.id, &s.label)));
    Value::Array(options)
}

/// Identity options for the user panel: the first is unspecified (empty value).
pub fn identity_options() -> Value {
    let mut options = vec![option("", "不指定")];
    for scene in &CATALOG.scenes {
        options.extend(scene.relationships.iter().map(|r| {
            option(
                &format!("{}/{}", scene.id, r.id),
                &format!("{} / {}", scene.label, r.label),
            )
        }));
    }
    Value::Array(options)
}

fn option(value: &str, label: &str) -> Value {
    json!({"value": value, "label": label})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_parsing() {
        let choice = parse_identity("work/peer").unwrap();
        assert_eq!(choice.scene.id, "work");
        assert_eq!(choice.label(), "工作 / 同事");
        assert!(parse_identity("none").is_none());
        assert!(parse_identity("work/nope").is_none());
        assert!(parse_identity("nope/peer").is_none());
    }

    #[test]
    fn default_scene_falls_back_to_first() {
        assert_eq!(default_scene(&json!({})).id, "general");
        assert_eq!(default_scene(&json!({"default_scene": "work"})).id, "work");
        let choice = SceneChoice {
            scene: default_scene(&json!({"default_scene": "work"})),
            relationship: None,
        };
        assert_eq!(choice.label(), "工作（不指定关系）");
        assert_eq!(choice.note(), choice.scene.unspecified_note);
    }

    #[test]
    fn options_start_with_empty_value() {
        let scenes = scene_options(&json!({"default_scene": "work"}));
        assert_eq!(scenes[0]["value"], "");
        assert_eq!(scenes[0]["label"], "默认（工作）");
        let identities = identity_options();
        assert_eq!(identities[0]["value"], "");
        // Every option's value can be parsed back into an identity.
        for option in identities.as_array().unwrap().iter().skip(1) {
            assert!(parse_identity(str_of(option, "value")).is_some());
        }
    }
}
