//! Physical key adjacency per layout, used to price typo substitutions.

/// Keyboard layouts the correction cost model understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutId {
    Colemak,
    Qwerty,
    /// On-screen keyboard with tap typing; uses QWERTY geometry.
    OskTap,
}

/// Main letter rows. Non-letter slots keep their column so geometry lines up.
const QWERTY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const COLEMAK_ROWS: [&str; 3] = ["qwfpgjluy;", "arstdhneio", "zxcvbkm"];

const SAME_KEY_COST: f32 = 0.0;
const NEIGHBOR_COST: f32 = 0.4;
const OSK_NEIGHBOR_COST: f32 = 0.3;
const UNRELATED_COST: f32 = 1.0;

fn rows_for(layout: LayoutId) -> &'static [&'static str; 3] {
    match layout {
        LayoutId::Colemak => &COLEMAK_ROWS,
        LayoutId::Qwerty | LayoutId::OskTap => &QWERTY_ROWS,
    }
}

/// (row, column) of a letter on the layout's main rows, or `None` for non-letters.
fn position(layout: LayoutId, c: char) -> Option<(usize, usize)> {
    if !c.is_ascii_alphabetic() {
        return None;
    }
    let c = c.to_ascii_lowercase();
    rows_for(layout)
        .iter()
        .enumerate()
        .find_map(|(row, keys)| keys.chars().position(|k| k == c).map(|col| (row, col)))
}

/// Substitution cost of typing `b` where `a` was intended, on `layout`.
///
/// 0.0 for the same character, the neighbor cost for physically adjacent keys
/// (0.4, or 0.3 on OskTap), and 1.0 otherwise. Case-insensitive.
pub fn sub_cost(layout: LayoutId, a: char, b: char) -> f32 {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    if a == b {
        return SAME_KEY_COST;
    }
    let (Some((ra, ca)), Some((rb, cb))) = (position(layout, a), position(layout, b)) else {
        return UNRELATED_COST;
    };
    let same_row = ra == rb && ca.abs_diff(cb) == 1;
    let adjacent_row = ra.abs_diff(rb) == 1 && ca.abs_diff(cb) <= 1;
    if same_row || adjacent_row {
        match layout {
            LayoutId::OskTap => OSK_NEIGHBOR_COST,
            _ => NEIGHBOR_COST,
        }
    } else {
        UNRELATED_COST
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colemak_neighbors_differ_from_qwerty() {
        // On Colemak, 'e' sits where QWERTY 'k' is; its row neighbors are 'n' and 'i'.
        assert_eq!(sub_cost(LayoutId::Colemak, 'e', 'i'), 0.4);
        assert_eq!(sub_cost(LayoutId::Qwerty, 'e', 'i'), 1.0);
        assert_eq!(sub_cost(LayoutId::Qwerty, 'e', 'r'), 0.4);
    }

    #[test]
    fn osk_uses_qwerty_geometry_cheaper() {
        assert_eq!(sub_cost(LayoutId::OskTap, 'e', 'r'), 0.3);
    }

    #[test]
    fn same_char_zero_other_full() {
        assert_eq!(sub_cost(LayoutId::Qwerty, 'a', 'a'), 0.0);
        assert_eq!(sub_cost(LayoutId::Qwerty, 'a', 'p'), 1.0);
    }
}
