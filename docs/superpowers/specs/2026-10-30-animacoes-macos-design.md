# Barra de tarefas: animações no padrão macOS, mais leve que a atual

Data: 2026-10-30 · Projeto: `~/Projetos/taskbar` (applet `io.github.Aenes0.CosmicTaskbar` no painel do COSMIC) · Branch: `main` (escolha do usuário; nada é commitado por Claude)

## Intenção

O usuário quer que a barra de tarefas que já existe no painel (não um dock) tenha o acabamento
visual e as animações do macOS, com capricho total, e que seja extremamente leve. Uma tentativa
anterior (`~/Projetos/cosmic-cosmetics`, módulo `bar`) foi abandonada; seu código serve de fonte de
curvas e ideias, não de base.

## Objetivos

1. **Zoom por proximidade.** O ícone sob o mouse cresce; os vizinhos crescem menos, com queda suave
   (curva tipo macOS), com mola ao entrar e ao sair.
2. **Salto no clique.** O ícone salta do lugar e volta com amortecimento. Ao abrir um app fechado,
   continua saltando até a janela aparecer.
3. **Prévias** com a mesma qualidade visual (cantos, sombra, título) e entrada/saída animada a
   partir do ícone (escala e fade). Mantêm as miniaturas ao vivo atuais.
4. **Janelas**: minimizar, restaurar e maximizar animados como no macOS. A janela sai de dentro do
   ícone e volta para ele; maximizar/restaurar interpola entre os retângulos.
5. **Leveza**: ver "Orçamento".

## Não objetivos

- Não vira dock. Continua applet do painel, como hoje, sem mudar a config do painel (tamanho S,
  embaixo, fundo transparente, `exclusive_zone` verdadeiro).
- Não mexe em Início, bandeja, Central nem em outros applets.
- Não troca o formato dos apps fixados (continuam compartilhados com a bandeja do dock do COSMIC).

## Anti-objetivos (seria falha mesmo que "funcione")

- Aumentar o consumo parado (hoje: PSS 16 MB, RSS 23 MB, 36 threads, 0% de CPU).
- Quadros agendados com a barra parada.
- Piscada ou "pulo" visível na troca entre o ícone do painel e o ícone ampliado.
- Animação de janela que trava ou deixa janela presa (minimizada sem volta).

## Restrições

- O applet vive na superfície do painel, de altura fixa e reservada pelo compositor. Não pode
  desenhar acima dela.
- Animação de janela só pode ocorrer dentro do compositor. O usuário já mantém o
  `cosmic-comp-aenes` (patches em `~/Projetos/cosmic-cosmetics/patches/cosmic-comp/`, build em
  `~/Projetos/cosmic-workspaces/build`). O patch `0005-window-animations` existe, **nunca foi
  compilado nem ligado** (ver `cosmic-cosmetics/docs/fork-comp/animacoes-janela.md`); a série
  0001-0004 está instalada.
- libcosmic/iced fixado como em `cosmic-applets epoch-1.10.0` (ver `Cargo.toml`); o app é
  `app.rs` (2815 linhas) mais `wayland_handler.rs`. As prévias já são popups xdg do applet.
- Regra do usuário: Claude não commita; testes na tela real só com aviso ("pode testar na tela").

## Decisões do usuário

- Continua applet, direto em `main`.
- **Zoom em superfície sobreposta:** o ícone ampliado é desenhado numa superfície transparente por
  cima do painel (mesmo mecanismo dos popups de prévia); os ícones do painel ficam parados por
  baixo. *Aprovado como direção; o mecanismo exato é validado pelo teste descartável.*
- Animações de janela no compositor, com o applet informando o retângulo de cada ícone.

## Decisões minhas (marcadas como suposição, o usuário pode trocar)

- Efeito de minimizar: genie simplificado (escala até ~0,1 com translação até o ícone, sem malha),
  como no patch 0005. Genie com malha fica fora.
- Orçamento de leveza (a confirmar): parado igual ou menor que hoje; durante animação, apenas
  desenhar os ícones afetados com damage mínimo; sem novas threads permanentes. Números-alvo de CPU
  por animação ficam para o plano, depois de medir o custo real do teste descartável.
- Backend de render: o `Cargo.toml` não liga `wgpu` ao libcosmic (o `Cargo.lock` ainda lista
  `iced_wgpu` entre os crates). A primeira tarefa do plano é confirmar qual backend o binário
  instalado realmente usa; a escolha entre manter iced/libcosmic e um renderizador próprio na
  superfície de zoom depende disso e do custo medido.

## Decisões fechadas após a revisão do documento (minhas, marcadas)

- **Ícone ampliado nítido:** re-rasterizado no tamanho final (SVG/PNG do tema), com cache por
  (ícone, tamanho em passos discretos); não se escala a textura pequena. *Minha decisão.*
- **Superfície de zoom sob demanda:** criada ao primeiro hover (aquecida enquanto o mouse se
  aproxima), destruída após um tempo parada sem o mouse. Parado = nenhuma superfície extra. Se a
  latência do primeiro quadro for visível no spike, passa a viver permanente com buffer mínimo
  vazio. *Minha decisão.*
- **Critério do spike (aprovado/reprovado):** aprovado se, em teste na tela real, (a) não há piscada
  na troca painel/sobreposição, (b) o mouse passa entre as duas sem cair clique nem hover,
  (c) o custo de CPU por quadro durante o zoom é medido e registrado. Reprovado em qualquer um.
- **Fase 4 (compositor) é entrega independente e atrasável.** As fases 2 e 3 valem sozinhas. O
  patch 0005 é a base; o plano o corrige. Retângulo do ícone: preferir o hint de retângulo do
  protocolo de toplevel existente do COSMIC; só se não servir, estender o protocolo (patch no
  `cosmic-protocols` já mantido).
- **Degradação segura no compositor:** sem retângulo informado (applet fechado, app sem ícone na
  barra, monitor sem o applet), a janela minimiza/restaura como hoje, sem animação. Nunca
  minimiza sem volta.
- **Monitores:** assume multi-monitor com escala; posição da sobreposição e retângulos são por
  saída.
- **Verde das fases:** curvas e molas em testes automáticos; zoom, salto e superfície só na tela
  real, com aviso ao usuário antes ("pode testar na tela"), com checklist por fase.
- **Movimento reduzido:** respeita a chave de duração de animação do `cosmic-comp-aenes` (patch 0002,
  `animation_duration_ms`; 0 = sem animação) e o mesmo padrão de `COSMETICS_REDUCE_MOTION`.
- **Plano B se o spike reprovar (decisão do usuário):** cortar o zoom por proximidade; ficam salto
  no clique, prévias e animações de janela. Não aumentar a altura do painel.

## Fases (ordem sugerida)

0. **Teste descartável (spike):** superfície transparente sobre o painel desenhando um ícone
   ampliado que segue o mouse. Responde: sem piscada? o mouse passa bem entre painel e
   sobreposição? custo de CPU por quadro? Nada daqui é mantido sem nova aprovação.
1. Medição de base (script reproduzível: PSS, RSS, threads, fds, CPU parado e em animação).
2. Zoom por proximidade + salto no clique (curvas e molas isoladas e testadas).
3. Prévias: entrada/saída animada e acabamento visual.
4. Compositor: compilar e ligar o patch 0005; protocolo para o retângulo do ícone;
   minimizar/restaurar/maximizar.
5. Revisão final e medição de leveza contra a base.

## O que fica a cargo de quem constrói

Curvas exatas (raio de influência, escala máxima, constantes de mola), duração das animações,
nomes de módulos, divisão do `app.rs`, formato do protocolo para o retângulo, e como medir.

## Riscos

- Superfície sobreposta pode piscar ou roubar o mouse (mitigação: teste descartável primeiro).
- Patch 0005 foi escrito sem o código do compositor à mão; pode não aplicar ou não compilar.
- Trocar o compositor exige novo pacote `cosmic-comp-aenes` e novo login.
