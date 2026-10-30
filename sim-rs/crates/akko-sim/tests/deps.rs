//! The simulator's references are the keyboard, the chip's and ARM's manuals,
//! and the Renode sim it replaced (its models and golden traces), never our
//! own PAC, HAL or firmware crates: a model built from them would agree with
//! any mistake in them (BOARD_FACTS.md). Cargo.lock lists every crate the
//! workspace builds, so check it.

const MEMBERS: [&str; 3] = ["akko-sim", "cortex-m3-emu", "wb32-sim"];
/// The repo's own crates, which could also arrive from crates.io or git.
const BANNED: [&str; 7] = [
    "wb32-pac",
    "wb32-hal",
    "akko-boot-guard",
    "akko-lighting",
    "qmk-wireless",
    "firmware",
    "wb32-rom-dfu",
];

#[test]
fn no_crate_of_ours_under_test() {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("sim-rs/Cargo.lock");
    let mut bad = Vec::new();
    for pkg in lock.split("[[package]]").skip(1) {
        let field = |key: &str| {
            pkg.lines().find_map(|l| {
                l.strip_prefix(key)?
                    .strip_prefix(" = \"")?
                    .strip_suffix('"')
            })
        };
        let name = field("name").expect("every package has a name");
        let from_path = field("source").is_none();
        let banned =
            name.starts_with("akko-") && !MEMBERS.contains(&name) || BANNED.contains(&name);
        if banned || from_path && !MEMBERS.contains(&name) {
            bad.push(name.to_string());
        }
    }
    assert!(bad.is_empty(), "sim-rs must not build {bad:?}");
}
