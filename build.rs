
#[cfg(windows)]
fn main() {
    
    let mut res = winres::WindowsResource::new();
    
    
    res.set_icon("icon.ico");
    
    
    if let Err(e) = res.compile() {
        eprintln!("Не удалось встроить иконку: {}", e);
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}