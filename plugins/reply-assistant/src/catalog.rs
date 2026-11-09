//! Analysis question bank ported from jev (https://github.com/jev-chat/jev-chat-jarvis, MIT
//! license). The English questions and criteria are identical to what jev sends; the Chinese labels
//! are used only for display.

use std::sync::LazyLock;

use zulip_plugin_sdk::serde_json::{self, Value};
use zulip_plugin_sdk::str_of;

/// A jev scene (work, romance, friends, family, general).
#[derive(Debug, PartialEq)]
pub struct Scene {
    pub id: String,
    pub label: String,
    pub unspecified_note: String,
    pub relationships: Vec<Relationship>,
    pub friction_examples: Vec<Vec<String>>,
    pub questions: Vec<Question>,
}

/// A relationship within a scene; `note` is the English description handed to the model.
#[derive(Debug, PartialEq)]
pub struct Relationship {
    pub id: String,
    pub label: String,
    pub note: String,
}

/// A yes/no question (kind is behavior or tone, role is action / passive / cue); when
/// `relationships` is not `None`, it is asked only under those relationships.
#[derive(Debug, PartialEq)]
pub struct Question {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub role: String,
    pub question: String,
    pub yes: String,
    pub no: String,
    pub relationships: Option<Vec<String>>,
}

/// A level of conflict (0 to 5).
#[derive(Debug, PartialEq)]
pub struct Level {
    pub label: String,
    pub what: String,
}

#[derive(Debug)]
pub struct Catalog {
    pub data_note: String,
    pub friction_other_question: String,
    pub friction_self_question: String,
    pub friction_levels: Vec<Level>,
    /// In jev's order: general, work, romance, friends, family.
    pub scenes: Vec<Scene>,
}

impl Catalog {
    pub fn scene(&self, id: &str) -> Option<&Scene> {
        self.scenes.iter().find(|s| s.id == id)
    }
}

pub static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let root = serde_json::from_str(include_str!("../jev-catalog.json")).unwrap_or(Value::Null);
    parse(&root)
});

fn text(value: &Value, key: &str) -> String {
    str_of(value, key).to_owned()
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn strings(value: &Value) -> Vec<String> {
    items(value)
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn parse(root: &Value) -> Catalog {
    let friction = &root["friction"];
    Catalog {
        data_note: text(root, "data_note"),
        friction_other_question: text(friction, "other_question"),
        friction_self_question: text(friction, "self_question"),
        friction_levels: items(&friction["levels"])
            .map(|l| Level {
                label: text(l, "label"),
                what: text(l, "what"),
            })
            .collect(),
        scenes: items(&root["scenes"]).map(parse_scene).collect(),
    }
}

fn parse_scene(scene: &Value) -> Scene {
    Scene {
        id: text(scene, "id"),
        label: text(scene, "label"),
        unspecified_note: text(scene, "unspecified_note"),
        relationships: items(&scene["relationships"])
            .map(|r| Relationship {
                id: text(r, "id"),
                label: text(r, "label"),
                note: text(r, "note"),
            })
            .collect(),
        friction_examples: items(&scene["friction_examples"]).map(strings).collect(),
        questions: items(&scene["questions"])
            .map(|q| Question {
                id: text(q, "id"),
                kind: text(q, "kind"),
                label: text(q, "label"),
                role: text(q, "role"),
                question: text(q, "question"),
                yes: text(q, "yes"),
                no: text(q, "no"),
                relationships: q["relationships"]
                    .is_array()
                    .then(|| strings(&q["relationships"])),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_complete() {
        let ids: Vec<&str> = CATALOG.scenes.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["general", "work", "romance", "friends", "family"]);
        assert_eq!(CATALOG.friction_levels.len(), 6);
        assert!(!CATALOG.data_note.is_empty());
        for scene in &CATALOG.scenes {
            assert!(!scene.questions.is_empty(), "{}", scene.id);
            assert!(!scene.relationships.is_empty(), "{}", scene.id);
            assert!(scene.questions.iter().all(|q| !q.question.is_empty()));
        }
    }
}
