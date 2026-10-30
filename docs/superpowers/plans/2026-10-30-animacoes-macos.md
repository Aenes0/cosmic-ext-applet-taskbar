# Animações macOS na barra de tarefas Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** zoom por proximidade, salto no clique, prévias animadas e janelas que sobem/descem do ícone, no applet existente, sem aumentar o consumo parado.

**Architecture:** um módulo puro `anim.rs` (molas e curvas, testável) alimenta duas superfícies: o zoom, numa popup transparente sobre o painel (mesmo mecanismo de `open_preview`, `grab = false`), e o salto/prévia no próprio applet. Quadros só existem enquanto algo anima. As animações de janela são uma fase separada no `cosmic-comp-aenes`.

**Tech Stack:** Rust 2024, libcosmic/iced (rev do `Cargo.toml`), cosmic-protocols `c0cff4d`, patch `0005-window-animations` do `cosmic-cosmetics`.

**Spec:** `docs/superpowers/specs/2026-10-30-animacoes-macos-design.md`

## Global Constraints

- Applet no painel, sem mudar a config do painel (tamanho S, embaixo, `exclusive_zone` verdadeiro).
- Parado: nenhum quadro agendado, nenhuma superfície extra; base medida: PSS 16 MB, RSS 23 MB, 36 threads, 0% CPU.
- Claude não commita nem faz push: todo trabalho fica no working tree em `main` (os passos "Entregar" substituem o commit).
- Teste na tela real só com aviso ao usuário ("pode testar na tela").
- Ícone ampliado re-rasterizado no tamanho final (passos discretos de tamanho, com cache); nunca escalar a textura pequena.
- Respeitar `animation_duration_ms` (0 = sem animação) e `COSMETICS_REDUCE_MOTION=1`.
- Plano B do zoom se o spike reprovar: cortar o zoom (não aumentar o painel).
- Subagentes: "não commite"; licença de corrigir o plano se o código do brief contradisser as assinaturas reais.

## Review Focus

1. Mouse sai da barra de repente com o zoom ativo: zoom volta a 1 e a popup some, sem quadro preso.
2. Janela abre/fecha (lista de ícones muda) durante o zoom: índices não ficam velhos.
3. Passar rápido por vários ícones: nenhuma popup órfã, no máximo uma de zoom.
4. `animation_duration_ms = 0` ou movimento reduzido: tudo instantâneo, zero quadros.
5. Painel em outra âncora (esquerda/direita/topo): a direção do zoom e do salto acompanha.

---

### Task 1: Medição de base

**Files:**
- Create: `scripts/measure.sh`

**Interfaces:**
- Produces: `scripts/measure.sh [pid]` imprime `PSS_kB RSS_kB THREADS FDS CPU%` do applet (pid padrão: `pgrep -f cosmic-ext-applet-taskbar`), via `flatpak-spawn --host` quando dentro do sandbox.

- [ ] **Step 1:** Escrever o script (lê `/proc/<pid>/status`, soma `Pss:` de `smaps`, conta `fd`, `ps -o pcpu`).
- [ ] **Step 2:** Rodar: `sh scripts/measure.sh` → saída com 5 números; PSS perto de 16 000 kB (base de 30/10).
- [ ] **Step 3:** Gravar a saída em `docs/superpowers/plans/baseline-2026-10-30.txt`.

### Task 2: Módulo de animação puro

**Files:**
- Create: `src/anim.rs`
- Modify: `src/main.rs` (declarar `mod anim;`)
- Test: `#[cfg(test)]` dentro de `src/anim.rs` (o crate ainda não tem testes)

**Interfaces:**
- Produces:
  - `pub const MAX_ZOOM: f32 = 1.7;` e `pub const ZOOM_RADIUS_ICONS: f32 = 2.5;`
  - `pub fn zoom_scale(distance_px: f32, icon_px: f32) -> f32` (1.0 a `MAX_ZOOM`, queda por cosseno; 1.0 a partir de `ZOOM_RADIUS_ICONS * icon_px`)
  - `pub struct Spring { pub pos: f32, pub vel: f32 }` com `pub fn new(pos: f32) -> Self`, `pub fn step(&mut self, target: f32, dt_s: f32, stiffness: f32, damping: f32)` e `pub fn settled(&self, target: f32) -> bool` (|pos-target| < 0.002 e |vel| < 0.01)
  - `pub fn bounce_offset(t_s: f32, icon_px: f32) -> f32` (deslocamento para cima em px: salto de ~0,5 ícone, 2 quedas amortecidas, 0 após ~0,9 s)
  - `pub fn motion_scale() -> f32` (0.0 se `COSMETICS_REDUCE_MOTION=1` ou `animation_duration_ms` = 0; senão 1.0)

- [ ] **Step 1:** Testes: `zoom_scale(0.0, 32.0) == MAX_ZOOM`; `zoom_scale(80.0, 32.0) == 1.0` (raio 2,5 ícones); monotônico decrescente entre 0 e 80; `Spring` converge a `target` em < 2 s a 120 Hz sem passar de 8% de sobra; `bounce_offset(0.0, 32.0) == 0.0`, pico entre 12 e 20 px, `bounce_offset(1.0, 32.0) == 0.0`.
- [ ] **Step 2:** Rodar `cargo test anim` (no host: `flatpak-spawn --host sh -c 'cd ~/Projetos/taskbar && cargo test anim'`) → falha por funções inexistentes.
- [ ] **Step 3:** Implementar as assinaturas acima (cosseno para o zoom; mola semi-implícita de Euler com passo fixo se `dt` > 1/30 s; salto como duas parábolas amortecidas).
- [ ] **Step 4:** Rodar `cargo test anim` → verde, contar testes.
- [ ] **Step 5:** Entregar: arquivos no working tree.

### Task 3: Spike do zoom (DESCARTÁVEL)

**Files:**
- Modify: `src/app.rs` (`PopupType::Zoom`, `Message::ZoomHover`, uso de `anim::zoom_scale`), marcar tudo com `// SPIKE`
- Reuse: padrão de `open_preview` (`app.rs:1017`), `self.rectangles`

**Interfaces:**
- Consumes: `anim::zoom_scale`, `anim::Spring`
- Produces: o veredito do spike; nada daqui é mantido sem aprovação.

- [ ] **Step 1:** Ao entrar o mouse na barra, abrir uma popup transparente (`grab = false`, sem teclado) ancorada sobre a fileira, com altura de `icon_px * MAX_ZOOM + sombra`, desenhando só os ícones ampliados conforme a posição do mouse; os ícones do painel ficam por baixo.
- [ ] **Step 2:** Pedir ao usuário "pode testar na tela" e checar: (a) sem piscada ao entrar/sair; (b) clique e hover passam para o ícone certo; (c) rodar `scripts/measure.sh` durante o zoom e anotar CPU.
- [ ] **Step 3:** Registrar o veredito (aprovado/reprovado, números) em `docs/superpowers/plans/spike-zoom-2026-10-30.md`. Reprovou: remover o código `// SPIKE`, seguir sem Task 4 (plano B), seguir direto às Tasks 5+.

### Task 4: Zoom definitivo (só se o spike aprovar)

**Files:**
- Create: `src/zoom.rs` (estado do zoom: escalas por item, `Spring` por item, ciclo de vida da popup)
- Modify: `src/app.rs` (ligar `zoom.rs`; apagar o `// SPIKE`), `src/main.rs`

**Interfaces:**
- Consumes: `anim::{zoom_scale, Spring, motion_scale}`, `self.rectangles`
- Produces: `pub struct Zoom` com `pub fn pointer_moved(&mut self, x: f32, rects: &[(DockItemId, Rectangle)])`, `pub fn tick(&mut self, dt_s: f32) -> bool` (true se ainda anima), `pub fn scale_of(&self, id: DockItemId) -> f32`, `pub fn pointer_left(&mut self)`

- [ ] **Step 1:** Testes em `zoom.rs`: `pointer_moved` sobre o ícone 2 de 5 deixa `scale_of(2) > scale_of(1) > scale_of(0)` depois de ticks; `pointer_left` leva todas a 1.0 e `tick` devolve `false`; lista que encolhe de 5 para 3 durante o zoom não causa pânico (Review Focus 2); `motion_scale() == 0.0` faz `scale_of` valer 1.0 e `tick` devolver `false` na hora (Review Focus 4).
- [ ] **Step 2:** Rodar `cargo test zoom` → falha.
- [ ] **Step 3:** Implementar `Zoom`; cache de ícone rasterizado por (nome, tamanho em passos de 4 px).
- [ ] **Step 4:** `cargo test` completo → verde, contar testes.
- [ ] **Step 5:** Teste na tela (com aviso): zoom suave, sem popup órfã ao varrer rápido (Review Focus 3), âncora lateral (Review Focus 5) se o usuário quiser testar. `scripts/measure.sh` parado: PSS ≤ base.

### Task 5: Salto no clique

**Files:**
- Create: `src/bounce.rs` (estado do salto por item; enquanto o app abre, repete até aparecer janela)
- Modify: `src/app.rs` (disparo no `Message::Exec` e `Message::GroupClick`; offset vertical no `as_icon`, `app.rs:167`)

**Interfaces:**
- Consumes: `anim::{bounce_offset, motion_scale}`
- Produces: `pub struct Bounce` com `pub fn start(&mut self, id: DockItemId, now_s: f32, repeat_until_window: bool)`, `pub fn stop(&mut self, id: DockItemId)`, `pub fn offset(&self, id: DockItemId, now_s: f32) -> f32`, `pub fn active(&self, now_s: f32) -> bool`

- [ ] **Step 1:** Testes: `offset` volta a 0 após o salto simples; com `repeat_until_window` segue saltando em 3,0 s e para ao chamar `stop`; com `motion_scale() == 0.0` o offset é sempre 0.
- [ ] **Step 2:** `cargo test bounce` → falha.
- [ ] **Step 3:** Implementar `Bounce` e ligar: o offset sobe pelo espaço livre do painel (sem passar da superfície); a subscription de quadros só existe enquanto `active`.
- [ ] **Step 4:** `cargo test bounce` → verde.
- [ ] **Step 5:** Teste na tela (com aviso): clicar num app fechado salta até a janela abrir; parado, `scripts/measure.sh` sem aumento.

### Task 6: Prévias animadas

**Files:**
- Modify: `src/app.rs` (`open_preview` `app.rs:1017`, `view_window` `app.rs:2345`, `toplevel_button` `app.rs:576`)
- Reuse: `anim::Spring`

**Interfaces:**
- Consumes: `anim::{Spring, motion_scale}`

- [ ] **Step 1:** Entrada e saída da prévia: escala 0,9 para 1 e fade, em ~200 ms, partindo do ícone; a prévia não fecha antes do fim da animação de saída.
- [ ] **Step 2:** Acabamento: cantos arredondados maiores, sombra e título no estilo macOS (valores finos a cargo do construtor, conferidos por captura de tela).
- [ ] **Step 3:** Teste na tela (com aviso): a captura mostra o visual; varrer vários apps com a prévia aberta continua seguindo na hora; sem prévia órfã (bug do `HoverSettled`, ver notas de 21/10).
- [ ] **Step 4:** `cargo test` completo → verde.

### Task 7 (entrega independente, atrasável): Janelas no compositor

**Files:**
- Modify: `~/Projetos/cosmic-cosmetics/patches/cosmic-comp/0005-window-animations.patch`, build em `~/Projetos/cosmic-workspaces/build`, `src/wayland_handler.rs` (envio do retângulo do ícone)

**Interfaces:**
- Consumes: `self.rectangles`; hint de retângulo do protocolo de toplevel existente (conferir `zcosmic_toplevel_handle_v1` do `cosmic-protocols c0cff4d`); só estender o protocolo se não servir.
- Produces: o compositor anima minimizar (genie simples), restaurar e maximizar até o retângulo; sem retângulo, comportamento original.

- [ ] **Step 1:** `patch -p1 --dry-run` do 0005 sobre upstream + 0001..0004; corrigir os hunks que não aplicam.
- [ ] **Step 2:** Compilar o comp (`cargo check`, depois `cargo build --release`) e rodar os 12 testes do módulo `window_fx` e os 3 de config.
- [ ] **Step 3:** Ligar `FxKind::Minimize/Open/Close` em map/unmap/minimize e aplicar `fx()` no render (os dois itens "falta ligar" de `animacoes-janela.md`).
- [ ] **Step 4:** O applet envia o retângulo de cada ícone ao compositor ao mudar `rectangles`.
- [ ] **Step 5:** Teste no compositor ANINHADO primeiro (`scripts/nested-test.sh` do `cosmic-workspaces`): minimizar, restaurar, janela sem retângulo, nenhuma janela presa minimizada (Review Focus implícito do spec).
- [ ] **Step 6:** Empacotar `cosmic-comp-aenes` (PKGBUILD) e **perguntar ao usuário** antes de instalar (exige novo login).

### Task 8: Revisão e medição final

- [ ] **Step 1:** `scripts/measure.sh` parado e durante animação; comparar com a base (Task 1) e registrar no spec.
- [ ] **Step 2:** `cargo test` e `cargo clippy` limpos; AgentShield; uma passada do `revisor` (fable) no diff.
- [ ] **Step 3:** Atualizar o README do taskbar (seção de animações) e lacunas no `gaps.md` e no vault.
