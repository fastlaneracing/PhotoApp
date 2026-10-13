use std::fs;
pub fn count(root: &str) -> (usize, usize) {
    let mut files = 0; let mut imgs = 0;
    let mut stack = vec![std::path::PathBuf::from(root)];
    while let Some(d) = stack.pop() {
        match fs::read_dir(&d) {
            Ok(rd) => for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { stack.push(p); }
                else {
                    files += 1;
                    let ext = p.extension().unwrap_or_default().to_string_lossy().to_lowercase();
                    if ext == "jpg" || ext == "png" { imgs += 1; }
                }
            },
            Err(err) => println!("read_dir FAIL {d:?}: {err}"),
        }
    }
    (files, imgs)
}
