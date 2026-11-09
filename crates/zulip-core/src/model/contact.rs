/// 好友分组。名称允许重复，以 `id` 区分。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendGroup {
    pub id: i64,
    pub name: String,
}
