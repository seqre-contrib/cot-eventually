use cot::db::{model, Auto};

#[model]
struct Kept {
    #[model(primary_key)]
    id: Auto<i32>,
}

#[model]
struct Removed {
    #[model(primary_key)]
    id: Auto<i32>,
}

fn main() {}
