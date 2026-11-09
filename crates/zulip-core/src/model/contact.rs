/// A friend group. Names may repeat; groups are distinguished by `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendGroup {
    pub id: i64,
    pub name: String,
}
