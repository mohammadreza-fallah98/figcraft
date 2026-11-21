use super::*;

#[test]
fn the_command_runs_this_program_with_the_link() {
    assert_eq!(open_command(r"C:\Program Files\FigIran\FigIran.exe"), r#""C:\Program Files\FigIran\FigIran.exe" "%1""#);
    assert_eq!(open_command(r"D:\my apps\FigIran.exe"), r#""D:\my apps\FigIran.exe" "%1""#);
}

#[test]
fn the_icon_is_the_programs_own() {
    assert_eq!(icon_value(r"C:\a b\FigIran.exe"), r#""C:\a b\FigIran.exe",0"#);
}

#[test]
fn a_cargo_build_folder_is_a_development_build() {
    assert!(is_development_build(r"D:\work\figcraft\target\debug\figiran.exe"));
    assert!(is_development_build(r"D:/work/figcraft/target/debug/figiran.exe"));
    assert!(is_development_build(r"D:\WORK\FIGCRAFT\TARGET\DEBUG\figiran.exe"));
    assert!(is_development_build(r"D:\work\target\release\deps\figiran-1234.exe"));
}

#[test]
fn a_program_run_from_anywhere_else_registers() {
    assert!(!is_development_build(r"C:\Users\someone\Downloads\FigIran.exe"));
    assert!(!is_development_build(r"D:\work\figcraft\release\FigIran.exe"));
    assert!(!is_development_build(r"C:\Program Files\FigIran\FigIran.exe"));
}

#[test]
fn the_opt_out_and_development_builds_skip() {
    assert!(skip(r"C:\Users\someone\FigIran.exe", true));
    assert!(skip(r"D:\work\target\debug\figiran.exe", false));
    assert!(!skip(r"C:\Users\someone\FigIran.exe", false));
}
