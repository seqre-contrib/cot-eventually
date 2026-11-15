use cot::db::{model, Auto, LimitedString};

const MAX_NAME_LENGTH: u32 = 64;

#[model]
struct Profile {
    #[model(primary_key)]
    id: Auto<i32>,
    name: LimitedString<MAX_NAME_LENGTH>,
}

fn main() {}
