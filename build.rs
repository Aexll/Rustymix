//! Intègre le logo à l'exécutable Windows (icône de l'explorateur et de la
//! barre des tâches). Rien à faire sur les autres systèmes : Linux utilise le
//! fichier `.desktop` et l'icône PNG fournis avec la version.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/logo.ico");
        resource.set("FileDescription", "Rustymix");
        resource.set("ProductName", "Rustymix");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=icône Windows non intégrée : {error}");
        }
    }
}
