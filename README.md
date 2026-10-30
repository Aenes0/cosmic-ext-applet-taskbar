> **Projeto depreciado.** A barra de tarefas e o menu Iniciar foram unificados em um programa só:
> **[Aenes0/cosmic-taskbar](https://github.com/Aenes0/cosmic-taskbar)**, que tem o histórico deste
> repositório e recebe todas as correções daqui em diante. Este repositório fica arquivado, somente leitura.

# Barra de tarefas — a bandeja de apps do Windows 11 para o COSMIC

> Fork da [bandeja de apps](https://github.com/pop-os/cosmic-applets/tree/master/cosmic-app-list)
> do COSMIC (`cosmic-app-list`, da System76, GPL-3.0), na versão 1.10.

Um applet para o painel do COSMIC que toma o lugar da bandeja de apps e se comporta como a
barra de tarefas do Windows 11:

- **prévia das janelas ao passar o mouse**: parado um instante sobre um app aberto, as
  janelas dele aparecem em miniatura, ao vivo (a imagem se atualiza a cada segundo), com o
  ícone, o título e, ao passar o mouse, o **×** para fechar. Clicar numa miniatura vai para
  a janela (ou a minimiza, se já está na frente); o botão do meio fecha. Indo de um app
  para o outro com as prévias abertas, elas seguem na hora; saindo, fecham. Elas não
  pegam o teclado nem prendem o mouse, como a dica de um botão;
- **uma fila só**: os fixados, depois os outros abertos, sem divisória;
- **a linha embaixo do app aberto**: curta e cinza com as janelas atrás, longa e na cor de
  destaque quando uma delas está na frente;
- **clique**: abre o app fechado; com uma janela, traz para a frente ou minimiza; com
  várias, mostra as janelas. O **botão do meio** abre uma nova;
- **clique direito**, a lista de atalhos na ordem do Windows: as ações do app (as do
  `.desktop`, como Nova janela anônima), o app (abre outro), **Fixar na barra de
  tarefas**/**Desafixar**, **Fechar janela**/**Fechar todas as janelas**;
- **contadores, progresso e atenção**, pelo mesmo D-Bus que os docks usam
  (`com.canonical.Unity.LauncherEntry`): as mensagens não lidas no canto do ícone, a barra
  de progresso de um download embaixo dele, e o botão em laranja quando o app pede
  atenção, até você olhar para ele;
- arrastar para reordenar e fixar, como na bandeja do COSMIC.

Os apps fixados são os mesmos da bandeja do dock: fixar num fixa no outro.

## Instalar

```sh
./install.sh            # compila e instala em ~/.local
./setup.sh              # troca a bandeja do COSMIC pela barra no painel
```

`./setup.sh --undo` volta a bandeja do COSMIC. Dá também para adicionar a **Barra de
tarefas** em Configurações › Área de trabalho › Painel › Applets.

Precisa de Rust 1.93 ou mais novo (o libcosmic pede) e das bibliotecas de
desenvolvimento do Wayland e do xkbcommon.

`COSMIC_TASKBAR_PREVIEW` (o id do `.desktop` de um app aberto) abre as prévias dele
sozinhas, uma vez, para tirar print.

A barra fala português e inglês, conforme o idioma do sistema. Licença GPL-3.0.
