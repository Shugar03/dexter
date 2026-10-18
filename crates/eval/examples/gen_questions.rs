use dexter_eval::{questions_dataset::generate_questions, to_jsonl};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/questions/items.jsonl");
    fs::create_dir_all(path.parent().expect("dataset path has parent"))?;
    let items = generate_questions();
    fs::write(&path, to_jsonl(&items)?)?;
    println!("wrote {} question items to {}", items.len(), path.display());
    Ok(())
}
