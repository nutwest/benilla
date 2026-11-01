//! Deviation: where the party's unit frames stand, for a consumer that outlines them. The outline is
//! the consumer's own overlay, so the stock frames stay as they are. It reads the frames' rects
//! with the 1.12 API each frame while something is selected.

use benilla_ui::script::UiScript;
use bevy::prelude::*;

use crate::target::PartySelection;

/// The frames, in the interface's 768-tall units, while something is selected.
#[derive(Resource, Default)]
pub struct PartyFrameRects(pub Vec<PartyFrameRect>);

/// A unit frame shown on screen: whose it is, and its rect from the screen's top-left.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyFrameRect {
    pub name: String,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// One line per shown frame: the unit's name, then its rect in screen units from the bottom-left.
const READ_FRAMES: &str = r#"
local out = {}
local function add(token, frame)
    local name = UnitName(token)
    if frame and name and frame:IsVisible() then
        local l, b, w, h = frame:GetRect()
        local s = frame:GetEffectiveScale()
        if l then
            table.insert(out, name .. "\t" .. l * s .. "\t" .. b * s .. "\t" .. w * s .. "\t" .. h * s)
        end
    end
end
add("player", PlayerFrame)
for i = 1, 4 do
    add("party" .. i, getglobal("PartyMemberFrame" .. i))
end
return table.concat(out, "\n")
"#;

/// The interface's height, whatever the window's.
const SCREEN_UNITS: f32 = 768.0;

/// [`READ_FRAMES`]'s answer as rects from the top-left; a line it can't read is left out.
fn parse(text: &str) -> Vec<PartyFrameRect> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.to_string();
            let mut number = || fields.next()?.parse::<f32>().ok();
            let (left, bottom, width, height) = (number()?, number()?, number()?, number()?);
            Some(PartyFrameRect {
                name,
                left,
                top: SCREEN_UNITS - bottom - height,
                width,
                height,
            })
        })
        .collect()
}

/// Reads the frames while something is selected, and clears them when nothing is.
pub(super) fn publish_party_frames(
    script: Option<NonSend<UiScript>>,
    selection: Res<PartySelection>,
    mut rects: ResMut<PartyFrameRects>,
) {
    if selection.0.is_empty() {
        if !rects.0.is_empty() {
            rects.0.clear();
        }
        return;
    }
    let Some(script) = script else { return };
    match script.eval::<String>(READ_FRAMES) {
        Ok(text) => rects.0 = parse(&text),
        Err(e) => warn!("ui_script: the party frames' rects: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_from_the_top_left() {
        let rects = parse("Partyone\t10\t700\t232\t68");
        assert_eq!(
            rects,
            [PartyFrameRect {
                name: "Partyone".into(),
                left: 10.0,
                top: 0.0,
                width: 232.0,
                height: 68.0,
            }]
        );
    }

    #[test]
    fn a_line_it_cannot_read_is_left_out() {
        let rects = parse("Partyone\t10\nPartytwo\t1\t2\t3\t4");
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].name, "Partytwo");
    }

    #[test]
    fn nothing_shown_reads_as_no_frames() {
        assert!(parse("").is_empty());
    }
}
