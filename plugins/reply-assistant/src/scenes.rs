//! 决定分析一条消息时使用的场景与关系：话题决定场景（没有设置时为默认场景），发送者的身份属于这个场景时
//! 才使用它的关系，否则不指定关系；私聊使用对方的身份（及其场景），对方没有身份时使用默认身份，
//! 也没有默认身份时为默认场景、不指定关系。身份写作“场景 id/关系 id”。

use zulip_plugin_sdk::serde_json::{Value, json};
use zulip_plugin_sdk::{Conversation, Message, str_of};

use crate::catalog::{CATALOG, Relationship, Scene};
use crate::store;

const DEFAULT_SCENE_KEY: &str = "default_scene";
const DEFAULT_IDENTITY_KEY: &str = "default_identity";

/// 分析时使用的场景与关系；`relationship` 为 `None` 表示不指定关系。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneChoice {
    pub scene: &'static Scene,
    pub relationship: Option<&'static Relationship>,
}

impl SceneChoice {
    /// 例如“工作 / 同事”或“工作（不指定关系）”。
    pub fn label(&self) -> String {
        match self.relationship {
            Some(relationship) => format!("{} / {}", self.scene.label, relationship.label),
            None => format!("{}（不指定关系）", self.scene.label),
        }
    }

    /// 交给模型的关系说明。
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
            // 一对一私聊为对方；多人私聊为发送者（自己发送的消息没有对方）。
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

/// 解析“场景 id/关系 id”；不存在时为 `None`。
pub fn parse_identity(identity: &str) -> Option<SceneChoice> {
    let (scene, relationship) = identity.split_once('/')?;
    let scene = CATALOG.scene(scene)?;
    let relationship = scene.relationships.iter().find(|r| r.id == relationship)?;
    Some(SceneChoice {
        scene,
        relationship: Some(relationship),
    })
}

/// 话题面板的场景选项：第一项为使用默认场景（值为空）。
pub fn scene_options(settings: &Value) -> Value {
    let default = format!("默认（{}）", default_scene(settings).label);
    let mut options = vec![option("", &default)];
    options.extend(CATALOG.scenes.iter().map(|s| option(&s.id, &s.label)));
    Value::Array(options)
}

/// 用户面板的身份选项：第一项为不指定（值为空）。
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
        // 每个选项的值都能解析回身份。
        for option in identities.as_array().unwrap().iter().skip(1) {
            assert!(parse_identity(str_of(option, "value")).is_some());
        }
    }
}
