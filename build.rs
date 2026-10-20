fn main() {
    topcoat::icon::iconify::BuildConfig::new()
        .icon_set("lucide")
        .stage()
        .expect("failed to stage Lucide icons");

    topcoat::tailwind::BuildConfig::new()
        .input("styles.css")
        .render()
        .expect("failed to generate Tailwind stylesheet");
}
