// SPDX-License-Identifier: GPL-3.0-only

//! The taskbar's words, in Portuguese or English, as the system's language is.

pub struct Texts {
    pub pin: &'static str,
    pub unpin: &'static str,
    pub close_window: &'static str,
    pub close_all: &'static str,
    pub run_on: &'static str,
    pub default_gpu: &'static str,
    pub untitled: &'static str,
}

const PT: Texts = Texts {
    pin: "Fixar na barra de tarefas",
    unpin: "Desafixar da barra de tarefas",
    close_window: "Fechar janela",
    close_all: "Fechar todas as janelas",
    run_on: "Abrir na {}",
    default_gpu: "(padrão)",
    untitled: "Sem título",
};

const EN: Texts = Texts {
    pin: "Pin to taskbar",
    unpin: "Unpin from taskbar",
    close_window: "Close window",
    close_all: "Close all windows",
    run_on: "Run on {}",
    default_gpu: "(default)",
    untitled: "Untitled",
};

pub fn texts() -> &'static Texts {
    let lang = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|l| !l.is_empty()))
        .unwrap_or_default();
    if lang.starts_with("pt") { &PT } else { &EN }
}
