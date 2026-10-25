use crate::Def;

#[path = "containers/join.rs"]
mod join;
#[path = "containers/pcod.rs"]
mod pcod;
#[path = "containers/remux.rs"]
mod remux;

pub fn defs() -> Vec<Def> {
    [remux::defs(), join::defs(), pcod::defs()]
        .into_iter()
        .flatten()
        .collect()
}
