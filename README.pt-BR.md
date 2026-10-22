<div align="center">

<img src="src-tauri/icons/128x128@2x.png" alt="" width="96">

# wStats

[English](README.md) · **Português**

**Monitor de sistema que vive dentro da barra de tarefas do Windows.**
Bonito, leve e sempre à vista.

<img src="docs/img/taskbar.png" alt="Os dez itens do wStats na barra de tarefas: CPU, GPU, NPU, memória, rede, disco, bateria, temperatura, ventoinha e consumo" width="840">

**Leve de verdade:** menos de 0,2% da CPU e, no modo econômico, cerca de 18 MB de memória.

</div>

---

## O que é

O wStats coloca pequenos gráficos de **CPU, GPU, NPU, memória, disco, rede, bateria, temperatura, ventoinhas e consumo de energia** direto na barra de tarefas. Clicar num item abre um painel com os detalhes: histórico de até 24 horas, processos, núcleos, sensores e atalhos para as ferramentas do Windows.

A diferença para outros monitores é que **o wStats faz parte da barra**: ele não flutua por cima dela. Os ícones de apps abrem espaço para ele como fazem com o botão de Widgets, e tudo se reorganiza quando você liga ou desliga um item.

E ele fica do seu jeito: **o nome de cada item vai em cima, embaixo, ao lado (à esquerda ou à direita) ou some**, e sem nome ao lado o número cresce para ocupar a altura da barra. Os gráficos também podem ser desligados, deixando só o texto.

<div align="center">
<img src="docs/img/pt-BR/labels.png" alt="Os quatro jeitos de mostrar a legenda: em cima, embaixo, ao lado e sem legenda" width="100%">
</div>

Para quem quer a barra livre, o **modo compacto** põe dois itens por coluna, só com os números, cada um identificado por um ícone ou pelo nome, à esquerda ou à direita. Os dez itens cabem em menos da metade do espaço, e cada número continua abrindo o seu painel.

<div align="center">
<img src="docs/img/pt-BR/compact.png" alt="O modo compacto nas quatro combinações: ícone ou legenda, à esquerda ou à direita" width="60%">
</div>

<div align="center">
<img src="docs/img/pt-BR/popups-dark.png" alt="Painéis de CPU, memória, NPU, temperatura e ventoinhas no tema escuro" width="100%">
<br><sub>Tema escuro: CPU, memória, NPU, temperatura e ventoinhas</sub>
<br><br>
<img src="docs/img/pt-BR/popups-light.png" alt="Painéis de rede, discos, alimentação e bateria no tema claro" width="100%">
<br><sub>Tema claro: rede, discos, alimentação e bateria</sub>
</div>

## Recursos

| Item | Na barra | No painel |
|---|---|---|
| **CPU** | uso com gráfico usuário/sistema | núcleos de desempenho e de eficiência, frequência, processos, GPU, tempo ligado |
| **GPU** | uso (mesmo critério do Gerenciador de Tarefas) | motores 3D/vídeo/cópia, memória dedicada e compartilhada, temperatura, ventoinha e clock quando o driver informa, consumo da placa (NVIDIA) |
| **NPU** | chip que enche com o uso; só aparece em máquinas com NPU (Intel AI Boost, AMD XDNA, Qualcomm Hexagon) | histórico de 24 h, memória, apps que estão usando a NPU |
| **Memória** | anel de uso | pressão, composição (apps, kernel, cache), processos, arquivo de paginação |
| **Discos** | espaço usado do C: | volumes, leitura/gravação com histórico, processos que mais leem e gravam |
| **Rede** | upload e download, na mesma unidade | histórico, Wi-Fi (SSID, sinal, canal, padrão), ping, IP público e local, tráfego da sessão |
| **Bateria** | carga, em pé; verde carregando, vermelha com 20% ou menos | saúde, ciclos, capacidade, potência de carga ou descarga, tempo restante |
| **Temperatura** | termômetro que sobe com a temperatura e muda de cor: na cor do tema até 70°, amarelo, laranja e vermelho a partir de 90° | cada núcleo, sensores da placa-mãe (desktop) ou da placa (Dell), GPU, histórico de 24 h |
| **Ventoinhas** | a mais rápida, em RPM | cada ventoinha (conectores da placa-mãe em desktops, notebooks Dell, GPU), histórico, perfil térmico da BIOS (Dell) |
| **Alimentação** | consumo da CPU em watts | consumo por componente (RAPL), GPU (NVIDIA), bateria, histórico de 24 h |

E também:

- **Tema claro e escuro**, seguindo o Windows ou fixo, com as duas cores dos gráficos configuráveis.
- **Histórico de 24 horas** com médias por minuto, guardado entre reinicializações (Agora / 1 h / 24 h em cada gráfico).
- **Posição** junto ao widget (entre o clima e o Iniciar) ou junto à bandeja, num monitor escolhido ou em todos.
- **Legenda do seu jeito**: em cima, embaixo, ao lado ou sem, com prévia ao vivo nas Configurações. Os gráficos também podem ser desligados, deixando só o texto.
- **Modo compacto**: dois itens por coluna, só com os números, identificados por ícone ou legenda, à esquerda ou à direita. Ocupa menos da metade do espaço.
- **Ordem dos itens** ajustável arrastando nas Configurações.
- **Português e inglês**, seguindo o idioma do Windows ou escolhido nas Configurações; o instalador também é bilíngue.
- **Economizar memória**: descarrega os painéis depois de 1 minuto sem uso, e o wStats cai para cerca de 18 MB (com os painéis sempre prontos, fica em torno de 110 MB, quase tudo do WebView2).
- **Iniciar com o Windows**, menu na bandeja e no clique direito da barra.

## Como funciona

```
 explorer.exe (barra de tarefas)                    wstats.exe
┌──────────────────────────────────┐   memória    ┌──────────────────────────────┐
│ wStatsTAP.dll  (stub, fixo)      │ compartilhada│ coletores: PDH, IP Helper,   │
│   └─ wStatsCore.dll (trocável)   │◄────────────►│ WLAN, DXGI/D3DKMT, bateria…  │
│        elemento XAML na barra    │   pixels +   │ desenha a faixa (Direct2D)   │
│        cliques → wStats          │   cliques    │ painéis em WebView2 (Tauri)  │
└──────────────────────────────────┘              └──────────────▲───────────────┘
                                                                 │ JSON, só leitura
                                                  ┌──────────────┴───────────────┐
                                                  │ wstats-sensors.exe (serviço) │
                                                  │ CPU, Super I/O (PawnIO), Dell│
                                                  └──────────────────────────────┘
```

- **Integração com a barra.** A barra do Windows 11 é XAML. O wStats usa a API oficial de diagnóstico de XAML (a mesma que o TranslucentTB usa) para carregar um pequeno stub dentro do Explorer. O stub carrega o código de verdade, que insere um elemento no layout da barra e ajusta as margens para os ícones abrirem espaço. O stub nunca muda, então atualizar o wStats troca o código dentro do Explorer **sem reiniciá-lo**. Cada barra (uma por monitor) roda na própria thread, com o próprio timer.
- **Desenho.** O wStats desenha a faixa com Direct2D e DirectWrite e publica os pixels numa memória compartilhada. O Explorer só exibe a imagem e devolve os cliques.
- **Plano B.** Se a injeção não for possível, o wStats usa uma janela encaixada no espaço livre da barra, medido com UI Automation, e tenta voltar para o XAML de tempos em tempos.
- **Sem admin.** O app roda como usuário comum. Só a temperatura e o consumo da CPU e as ventoinhas precisam de privilégios, e para isso há um serviço separado (abaixo).

### Serviço de sensores

Temperatura e consumo da CPU e ventoinhas não são acessíveis sem privilégios. Em vez de rodar o wStats inteiro como administrador, um serviço pequeno (`wstats-sensors.exe`, ~220 KB) roda como sistema e só publica leituras:

- **CPU Intel:** temperatura de cada núcleo e do pacote (MSRs térmicos).
- **CPU AMD (Zen, família 17h–1Ah):** Tctl e temperatura de cada CCD.
- **Consumo da CPU (Intel e AMD):** contadores de energia RAPL, quando o próprio Windows não os publica (desktops e toda CPU AMD).
- **Placas-mãe de desktop:** ventoinhas e temperaturas da placa pelo chip Super I/O: Nuvoton NCT6779D e mais novos (ASUS, ASRock, muitas MSI), NCT6683D/6686D/6687D (MSI recentes) e ITE IT86xx/IT87xx (Gigabyte). Só leitura; o controle das ventoinhas continua com a BIOS. Entradas auxiliares de temperatura que a BIOS não usa ficam ocultas, porque pinos desconectados leem lixo.
- **Dell:** ventoinhas e sensores da placa pela interface WMI oficial da Dell (DDV), e o perfil térmico da BIOS.

A leitura da CPU e do Super I/O usa o [PawnIO](https://github.com/namazso/PawnIO), um driver assinado que só executa módulos assinados. É o mesmo que o LibreHardwareMonitor e o FanControl usam. Nada do wStats roda no kernel, e o acesso ao Super I/O usa a mesma trava dos outros monitores, então eles convivem.

O serviço é ligado em **Configurações › Temperatura** (ou › Ventoinhas) com um clique e um UAC. Ele é copiado para `Program Files` antes de ser registrado e publica os dados numa seção de memória que qualquer usuário lê, mas só o sistema escreve.

## Instalação

Baixe o instalador na página de [Releases](https://github.com/rbferreira/wStats/releases) e execute (ou gere o seu com `npm run installer`). Um único pedido de administrador instala:

- o wStats em `Program Files\wStats`, iniciando com o Windows;
- se você aceitar (padrão), o **serviço de sensores** e, se ainda não estiver presente, o **driver PawnIO**, usando o setup oficial assinado que vem dentro do instalador.

Na desinstalação o serviço sai junto. O PawnIO fica, a menos que você peça, porque outros programas podem usá-lo.

## Desenvolvimento

**Requisitos:** Windows 11, [Visual Studio Build Tools 2022](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (C++), [Rust](https://rustup.rs) estável e Node.js 20+.

```bash
npm install
npx tauri dev                    # app em modo de desenvolvimento
npx tauri build --no-bundle      # build de release (src-tauri/target/release/wstats.exe)

cd sensors && cargo build --release   # serviço de sensores

npm run installer                # instalador NSIS em src-tauri/target/release/bundle/nsis
```

O `npm run installer` compila o serviço, baixa o setup do PawnIO (versão fixa, conferida por SHA-256 e assinatura) e empacota tudo com os hooks de `src-tauri/installer/hooks.nsh`.

O `build.rs` compila as DLLs do Explorer (`tap/`) automaticamente. Em modo debug, recompilar o `tap/` troca o código dentro do Explorer na hora.

Testes: `cargo test` em `src-tauri/` e em `sensors/`, e `npx tsc --noEmit` para o front.

Outras ferramentas:

- `preview.html` mostra todos os painéis com dados fictícios no navegador (`npx vite`, depois `/preview.html`).
- `WSTATS_STRIP=window` força o modo janela, para testar o plano B.

### Estrutura

```
src/                 painéis e Configurações (TypeScript, sem framework)
  modules/           um arquivo por item: cpu, gpu, npu, mem, disk, net, battery, temp, fans, power
src-tauri/src/
  stats/             coletores e histórico (PDH, IP Helper, WLAN, DXGI, D3DKMT, bateria, sensores)
  strip/             a faixa na barra: desenho, integração XAML e plano B
  popup.rs           posicionamento e ciclo de vida dos painéis
tap/                 DLLs que rodam dentro do Explorer (C++/WinRT)
sensors/             serviço de sensores (Rust)
docs/                referência de design e imagens do README
```

## Créditos

- Inspirado no [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor).
- Integração com a barra baseada na técnica do [TranslucentTB](https://github.com/TranslucentTB/TranslucentTB).
- [PawnIO](https://github.com/namazso/PawnIO) (GPL-2.0, setup oficial redistribuído sem modificação) e seus [módulos](https://github.com/namazso/PawnIO.Modules) (LGPL-2.1, ver `sensors/pawnio/COPYING`), por namazso.
- Formato dos dados de sensores da Dell conforme o driver `dell-wmi-ddv` do Linux.

## Licença

[MIT](LICENSE).
