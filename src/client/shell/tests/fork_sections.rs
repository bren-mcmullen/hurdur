use super::*;
use crossterm::event::{MouseButton, MouseEventKind};

const MR_ITEM: &str = r#"{"url":"https://gitlab.test/mr/153","lines":[[{"text":"es-infra !153 tag secret"}],[{"text":"  ◐ pipeline running","color":"blue"}]]}"#;
const ARTIFACT_ITEM: &str = r#"{"url":"https://claude.ai/artifact/abc","lines":[[{"text":"↗ AWS Access Model","color":"blue"}]]}"#;

fn state_with_sections(sections: &[(&str, &str)]) -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.fork_sections = super::super::fork_sections::ForkSections::from_jsonl_for_test(sections);
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn click(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

#[test]
fn fork_sections_render_below_agents_and_open_urls_on_click() {
    let mut state = state_with_sections(&[("mrs", MR_ITEM), ("artifacts", ARTIFACT_ITEM)]);
    let frame = state.compose(106, 30).expect("frame");
    let rows = frame_rows(&frame);

    let row_of = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing from frame:\n{}", rows.join("\n")))
    };
    assert!(row_of(" agents") < row_of(" mrs"));
    assert_eq!(row_of(" mrs") + 1, row_of("es-infra !153"));
    assert_eq!(row_of("es-infra !153") + 1, row_of("pipeline running"));
    assert!(row_of("pipeline running") < row_of(" artifacts"));
    assert_eq!(row_of("AWS Access Model"), 29);

    let mr_row = row_of("pipeline running") as u16;
    let opened = click(&mut state, 3, mr_row);
    assert!(matches!(
        &opened.actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)] if url == "https://gitlab.test/mr/153"
    ));
    let header_click = click(&mut state, 3, row_of(" mrs") as u16);
    assert!(header_click.actions.is_empty());
}

#[test]
fn empty_fork_sections_leave_the_sidebar_unchanged() {
    let mut plain = state_with_sections(&[]);
    let mut empty = state_with_sections(&[("mrs", "")]);
    let plain_rows = frame_rows(&plain.compose(106, 30).expect("frame"));
    let empty_rows = frame_rows(&empty.compose(106, 30).expect("frame"));
    assert_eq!(plain_rows, empty_rows);
    assert_eq!(
        plain.hits.sidebar_section_divider,
        empty.hits.sidebar_section_divider
    );
}

#[test]
fn section_divider_drag_tracks_the_reduced_sidebar_area() {
    let mut state = state_with_sections(&[("mrs", MR_ITEM), ("artifacts", ARTIFACT_ITEM)]);
    state.compose(106, 30).expect("frame");
    let divider = state.hits.sidebar_section_divider;
    let target_row = divider.y + 3;

    click(&mut state, divider.x + 1, divider.y);
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: divider.x + 1,
        row: target_row,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 30).expect("frame");
    assert_eq!(state.hits.sidebar_section_divider.y, target_row);
}

#[test]
fn fork_section_clicks_are_disabled_without_mouse_capture() {
    let mut state = state_with_sections(&[("mrs", MR_ITEM)]);
    state.config.mouse_capture = false;
    state.compose(106, 30).expect("frame");
    assert!(super::super::fork_sections::clicked_url(&state.hits, (3, 28)).is_none());
}
