pub mod graph_map;

use std::{
    io::Write,
    process::{Command, Stdio},
};

use color_eyre::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode},
    layout::{Constraint, Layout, Position},
    style::{Color, Style, Stylize},
    text::{Line, Text},
    widgets::{Block, Paragraph},
};

use crate::graph_map::{GraphMap, GraphNode};

fn main() -> Result<()> {
    color_eyre::install()?;
    let terminal = ratatui::init();
    let app_result = App::new().run(terminal);
    ratatui::restore();
    app_result?;
    Ok(())
}

#[derive(Debug)]
struct Node {}

impl Node {
    pub fn new() -> Self {
        Self {}
    }
}

impl GraphNode for Node {
    fn edge_added(&mut self, other: &Self, other_id: usize) {
        todo!()
    }

    fn edge_removed(&mut self, other: &Self, other_id: usize) {
        todo!()
    }
}

struct App {
    input: String,
    character_index: usize,
    graph: GraphMap<Node>,
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            character_index: 0,
            graph: GraphMap::new(),
        }
    }

    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.character_index.saturating_sub(1);
        self.character_index = self.clamp_cursor(cursor_moved_left);
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.character_index.saturating_add(1);
        self.character_index = self.clamp_cursor(cursor_moved_right);
    }

    fn enter_char(&mut self, new_char: char) {
        let index = self.byte_index();
        self.input.insert(index, new_char);
        self.move_cursor_right();
    }

    /// Returns the byte index based on the character position.
    ///
    /// Since each character in a string can be contain multiple bytes, it's necessary to calculate
    /// the byte index based on the index of the character.
    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.character_index)
            .unwrap_or(self.input.len())
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.character_index != 0;
        if is_not_cursor_leftmost {
            let current_index = self.character_index;
            let from_left_to_current_index = current_index - 1;
            let before_char_to_delete = self.input.chars().take(from_left_to_current_index);
            let after_char_to_delete = self.input.chars().skip(current_index);
            self.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.chars().count())
    }

    fn reset_cursor(&mut self) {
        self.character_index = 0;
    }

    fn submit_message(&mut self) {
        if self.input.len() >= 3 {
            let command = &self.input[0..3];
            match command {
                "add" => {
                    self.graph.add_node(Node::new());
                }
                "con" => {
                    if let Some((lhs, rhs)) = self.input[4..].split_once(' ')
                        && let Some(lhs) = lhs.parse::<usize>().ok()
                        && let Some(rhs) = rhs.parse::<usize>().ok()
                    {
                        self.graph.add_edge(lhs, rhs);
                    }
                }
                _ => {}
            }
        }
        self.input.clear();
        self.reset_cursor();
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> Result<Self> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter => self.submit_message(),
                    KeyCode::Char(to_insert) => self.enter_char(to_insert),
                    KeyCode::Backspace => self.delete_char(),
                    KeyCode::Left => self.move_cursor_left(),
                    KeyCode::Right => self.move_cursor_right(),
                    KeyCode::Esc => {
                        return Ok(self);
                    }
                    _ => {}
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let vertical = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(1),
        ]);
        let [help_area, input_area, messages_area] = vertical.areas(frame.area());

        let msg = vec![
            "Press ".into(),
            "Esc".bold(),
            " to quit, ".into(),
            "Enter".bold(),
            " to record the message".into(),
        ];
        let text = Text::from(Line::from(msg)).patch_style(Style::default());
        let help_message = Paragraph::new(text);
        frame.render_widget(help_message, help_area);

        let input = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(Color::Yellow))
            .block(Block::bordered().title("Input"));
        frame.render_widget(input, input_area);
        frame.set_cursor_position(Position::new(
            input_area.x + self.character_index as u16 + 1,
            input_area.y + 1,
        ));
        let dot = self.graph.dot_graph();
        let graph = {
            let mut graph = Command::new("graph-easy")
                .arg("--from=dot")
                .stdin(Stdio::piped())
                .stderr(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let stdin = graph.stdin.as_mut().unwrap();
            stdin.write_all(dot.as_bytes()).unwrap();
            graph
                .wait_with_output()
                .ok()
                .and_then(|x| String::from_utf8(x.stdout).ok())
                .unwrap_or_default()
        };
        let content = Paragraph::new(graph);
        frame.render_widget(content, messages_area);
    }
}
