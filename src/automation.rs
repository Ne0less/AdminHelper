use enigo::{Enigo, Key, KeyboardControllable}; // Убрали Settings
use std::{thread, time::Duration};

pub struct InputHandler {
    enigo: Enigo,
}

impl InputHandler {
    pub fn new() -> Self {
        
        Self {
            enigo: Enigo::new(), 
        }
    }

    pub fn send_command(&mut self, command: &str) {
        self.enigo.key_click(Key::Layout('t'));
        thread::sleep(Duration::from_millis(50));
        
        
        self.enigo.key_sequence(command);
        thread::sleep(Duration::from_millis(50));
        
        self.enigo.key_click(Key::Return);
        thread::sleep(Duration::from_millis(150));
    }

    pub fn send_multiple(&mut self, commands: &[String]) {
        for cmd in commands {
            self.send_command(cmd);
        }
        self.enigo.key_click(Key::Escape);
    }
}