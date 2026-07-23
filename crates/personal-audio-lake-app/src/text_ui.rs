use std::io::{self, BufRead, Write};

use anyhow::Result;

use crate::{
    Config,
    app_controller::AppController,
    text_view::{self, Input},
    view_model::ViewModel,
};

pub fn run(config: &Config, commands: &[String]) -> Result<()> {
    let controller = AppController::new(config.clone());
    let mut model = controller.load_model()?;

    if !commands.is_empty() {
        for command in commands {
            if !apply(command, &controller, &mut model) {
                break;
            }
        }
        print!("{}", text_view::render(&model));
        return Ok(());
    }

    println!("{}", text_view::render(&model));
    println!("Text view is temporary; type help for deterministic commands.");
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if !apply(&line, &controller, &mut model) {
            break;
        }
        println!("\n{}", text_view::render(&model));
        print!("> ");
        stdout.flush()?;
    }
    Ok(())
}

fn apply(command: &str, controller: &AppController, model: &mut ViewModel) -> bool {
    match text_view::parse(command) {
        Ok(Input::Quit) => return false,
        Ok(Input::Help) => println!("{}", text_view::help()),
        Ok(Input::Render) => {}
        Ok(Input::Intent(intent)) => match model.update(intent) {
            Ok(effects) => {
                if let Err(error) = controller.apply_all(effects, model) {
                    eprintln!("Could not apply command: {error:#}");
                }
            }
            Err(error) => eprintln!("Command rejected: {error}"),
        },
        Err(error) => eprintln!("Command rejected: {error}"),
    }
    true
}
