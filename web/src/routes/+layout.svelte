<script lang="ts">
  import '@fontsource-variable/instrument-sans/wght.css';
  import '@fontsource-variable/jetbrains-mono/wght.css';
  import '../app.css';
  import { goto, afterNavigate } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import { announceBuild, build, builtAt, shortRevision } from '$lib/build';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import Icon, { type IconName } from '$lib/components/Icon.svelte';
  import Toasts from '$lib/components/Toasts.svelte';
  import { i18n, t, setLocale, type MessageKey } from '$lib/i18n/index.svelte';
  import { connectLive, disconnectLive, live, setStatus } from '$lib/live.svelte';
  import { applyTheme, can, loadSession, session, ui } from '$lib/session.svelte';
  import type { InstanceSummary } from '$lib/types';

  let { children } = $props();

  const publicRoutes = ['/login', '/setup'];
  let isPublic = $derived(publicRoutes.includes(page.url.pathname));
  let ready = $state(false);
  let menuOpen = $state(false);
  let instances = $state<InstanceSummary[]>([]);

  async function loadInstances() {
    try {
      instances = await api.instances();
      for (const i of instances) setStatus(i.id, i.status);
    } catch {
      instances = [];
    }
  }

  onMount(async () => {
    announceBuild();
    applyTheme(ui.theme);
    setLocale(i18n.locale);
    const me = await loadSession();
    if (!me) {
      const { required } = await api.setupRequired().catch(() => ({ required: false }));
      if (!isPublic) await goto(required ? '/setup' : '/login', { replaceState: true });
    } else {
      connectLive();
      await loadInstances();
      if (isPublic) await goto('/', { replaceState: true });
    }
    ready = true;
  });

  afterNavigate(() => {
    menuOpen = false;
  });

  // Instances started/stopped elsewhere (other users, API) appear without a reload.
  $effect(() => {
    if (live.version > 0 && session.me) void loadInstances();
  });

  // Below 900 px the sidebar is a drawer: hidden from keyboard and screen readers when closed.
  let narrow = $state(false);
  $effect(() => {
    const query = matchMedia('(max-width: 899px)');
    narrow = query.matches;
    const update = () => (narrow = query.matches);
    query.addEventListener('change', update);
    return () => query.removeEventListener('change', update);
  });
  let sidebar = $state<HTMLElement>();
  $effect(() => {
    if (menuOpen) sidebar?.querySelector<HTMLElement>('a, button')?.focus();
  });

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && menuOpen) {
      menuOpen = false;
      document.querySelector<HTMLElement>('button[aria-controls="sidebar"]')?.focus();
    }
  }

  async function logout() {
    await api.logout().catch(() => {});
    disconnectLive();
    session.me = null;
    await goto('/login');
  }

  const nav: { href: string; icon: IconName; label: MessageKey; permission?: string; admin?: boolean }[] = [
    { href: '/', icon: 'dashboard', label: 'nav.dashboard' },
    { href: '/library', icon: 'library', label: 'nav.library', permission: 'library.read' },
    { href: '/playlists', icon: 'list', label: 'nav.playlists', permission: 'playlist.read' },
    { href: '/plugins', icon: 'puzzle', label: 'nav.plugins', permission: 'plugins.manage' },
    { href: '/users', icon: 'users', label: 'nav.users', permission: 'users.manage' },
    { href: '/settings', icon: 'settings', label: 'nav.settings', admin: true },
  ];

  function active(href: string): boolean {
    const path = page.url.pathname;
    return href === '/' ? path === '/' : path === href || path.startsWith(href + '/');
  }

  type DotState = 'playing' | 'paused' | 'connected' | 'connecting' | 'stopped';
  function stateOf(i: InstanceSummary): DotState {
    const status = live.instances[i.id]?.status;
    if (!status) return 'stopped';
    if (!status.connected) return 'connecting';
    if (!status.current) return 'connected';
    return status.paused ? 'paused' : 'playing';
  }

  function nowPlaying(i: InstanceSummary): string | null {
    const status = live.instances[i.id]?.status;
    return status?.current ? status.current.track.title : null;
  }

  // The tally marker slides to the active entry of the sidebar.
  let marker = $state<{ top: number; height: number } | null>(null);
  $effect(() => {
    void page.url.pathname;
    void instances.length;
    void session.me;
    void i18n.locale;
    const nav = sidebar;
    if (!nav) return;
    const place = () => {
      const link = nav.querySelector<HTMLElement>('a.active');
      marker = link ? { top: link.offsetTop, height: link.offsetHeight } : null;
    };
    place();
    // Links move when lists above them change height (now-playing lines, fonts, language).
    const observer = new ResizeObserver(place);
    observer.observe(nav);
    for (const list of nav.querySelectorAll('ul')) observer.observe(list);
    void document.fonts?.ready.then(place);
    return () => observer.disconnect();
  });

  const dotLabel: Record<DotState, MessageKey> = {
    playing: 'dashboard.playing',
    paused: 'dashboard.paused',
    connected: 'instance.connected',
    connecting: 'instance.connecting',
    stopped: 'instance.stopped',
  };
</script>

<svelte:window onkeydown={onKeydown} />

<a class="skip" href="#main">{t('nav.skip')}</a>

{#if !ready}
  <div class="splash" aria-busy="true">{t('common.loading')}</div>
{:else if isPublic || !session.me}
  {@render children()}
{:else}
  <div class="shell">
    <header class="topbar">
      <button class="icon-btn" onclick={() => (menuOpen = !menuOpen)} aria-expanded={menuOpen} aria-controls="sidebar">
        <Icon name={menuOpen ? 'x' : 'menu'} label={t('nav.menu')} />
      </button>
      <a href="/" class="brand"><span class="tally" aria-hidden="true"></span>James</a>
      {#if !live.connected}
        <span class="topbar-offline small" role="status"><Icon name="alert" size={16} />{t('common.reconnecting')}</span>
      {/if}
    </header>

    <nav
      id="sidebar"
      class="sidebar"
      class:open={menuOpen}
      aria-label={t('nav.menu')}
      bind:this={sidebar}
      inert={narrow && !menuOpen}
    >
      {#if marker}
        <span class="marker" aria-hidden="true" style:transform="translateY({marker.top}px)" style:height="{marker.height}px"></span>
      {/if}
      <a href="/" class="brand desktop-only"><span class="tally" aria-hidden="true"></span>James</a>

      <ul class="nav">
        <li>
          <a href="/" class:active={active('/')} aria-current={active('/') ? 'page' : undefined}>
            <Icon name="dashboard" size={18} />{t('nav.dashboard')}
          </a>
        </li>
      </ul>

      <div class="section-title">
        <span>{t('nav.instances')}</span>
        {#if can('instance.manage')}
          <a href="/instances/new" class="icon-btn small" aria-label={t('instance.new')} title={t('instance.new')}>
            <Icon name="plus" size={16} />
          </a>
        {/if}
      </div>
      <ul class="nav instances">
        {#each instances as instance (instance.id)}
          {@const dot = stateOf(instance)}
          {@const playing = nowPlaying(instance)}
          <li>
            <a
              href="/instances/{instance.id}"
              class:active={active(`/instances/${instance.id}`)}
              aria-current={active(`/instances/${instance.id}`) ? 'page' : undefined}
            >
              {#if dot === 'playing' || dot === 'paused'}
                <span class="meter" class:still={dot === 'paused'} aria-hidden="true"><i></i><i></i><i></i></span>
              {:else}
                <span class="status-dot {dot}" aria-hidden="true"></span>
              {/if}
              <span class="instance-text">
                <span class="truncate">{instance.name}</span>
                {#if playing}<span class="truncate sub">{playing}</span>{/if}
              </span>
              <span class="visually-hidden">({t(dotLabel[dot])})</span>
            </a>
          </li>
        {:else}
          <li class="none">{t('dashboard.empty')}</li>
        {/each}
      </ul>

      {#if nav.slice(1).some((n) => (!n.permission || can(n.permission)) && (!n.admin || session.me?.user.isAdmin))}
        <div class="section-title"><span>{t('nav.manage')}</span></div>
        <ul class="nav">
          {#each nav.slice(1).filter((n) => (!n.permission || can(n.permission)) && (!n.admin || session.me?.user.isAdmin)) as item (item.href)}
            <li>
              <a href={item.href} class:active={active(item.href)} aria-current={active(item.href) ? 'page' : undefined}>
                <Icon name={item.icon} size={18} />
                {t(item.label)}
              </a>
            </li>
          {/each}
        </ul>
      {/if}

      <div class="spacer"></div>

      {#if !live.connected}
        <p class="offline small" role="status"><Icon name="alert" size={16} />{t('common.reconnecting')}</p>
      {/if}

      <div class="account">
        <a href="/account" class="who" class:active={active('/account')} aria-current={active('/account') ? 'page' : undefined}>
          <span class="avatar" aria-hidden="true">{session.me.user.username.slice(0, 1).toUpperCase()}</span>
          <span class="truncate">{session.me.user.username}</span>
        </a>
        <button class="icon-btn small" onclick={logout} aria-label={t('nav.logout')} title={t('nav.logout')}><Icon name="logout" size={18} /></button>
      </div>
      <p class="build" title={build.revision ? `${t('nav.commit')} ${build.revision}` : undefined}>
        <span>v{build.version}{#if shortRevision}{' · '}<span class="mono">{shortRevision}</span>{/if}</span>
        <span>{t('nav.built', { time: builtAt(i18n.locale) })}</span>
      </p>
    </nav>
    {#if menuOpen}
      <button class="scrim" aria-label={t('common.close')} onclick={() => (menuOpen = false)}></button>
    {/if}

    <!-- While the drawer is open on small screens, the page behind it is not reachable. -->
    <main id="main" tabindex="-1" inert={narrow && menuOpen}>
      {#key page.url.pathname}
        {@render children()}
      {/key}
    </main>
  </div>
{/if}

<Toasts />
<ConfirmDialog />

<style>
  .skip {
    position: absolute;
    left: -9999px;
    top: 8px;
    z-index: 200;
    padding: var(--space-2) var(--space-3);
    background: var(--tally);
    color: var(--tally-ink);
    border-radius: var(--radius);
  }
  .skip:focus {
    left: 8px;
  }
  .splash {
    display: grid;
    place-items: center;
    min-height: 100vh;
    color: var(--text-muted);
  }
  .shell {
    min-height: 100vh;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-weight: 650;
    font-size: 1.1875rem;
    letter-spacing: -0.02em;
    color: var(--side-text);
  }
  .brand:hover {
    text-decoration: none;
  }
  /* The tally light: James's mark. */
  .tally {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    background: var(--tally);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--tally) 22%, transparent);
  }
  .topbar {
    position: sticky;
    top: 0;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 56px;
    padding: 0 max(var(--space-2), env(safe-area-inset-right)) 0 max(var(--space-2), env(safe-area-inset-left));
    background: var(--side-bg);
    color: var(--side-text);
    border-bottom: 1px solid var(--side-border);
  }
  .topbar .icon-btn,
  .sidebar .icon-btn {
    color: var(--side-muted);
  }
  .topbar .icon-btn:hover,
  .sidebar .icon-btn:hover {
    background: var(--side-surface);
    color: var(--side-text);
  }
  .sidebar {
    position: fixed;
    inset: 56px auto 0 0;
    width: min(var(--sidebar-width), 85vw);
    z-index: 30;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--space-3);
    background: var(--side-bg);
    color: var(--side-text);
    border-right: 1px solid var(--side-border);
    overflow-y: auto;
    transform: translateX(-105%);
    transition: transform var(--dur-3) var(--ease-out);
    --focus: #ff8a3d;
  }
  .sidebar.open {
    transform: none;
  }
  .marker {
    position: absolute;
    top: 0;
    left: 0;
    width: 3px;
    border-radius: 0 3px 3px 0;
    background: var(--tally);
    transition:
      transform 220ms var(--ease-out),
      height 220ms var(--ease-out);
  }
  .scrim {
    position: fixed;
    inset: 56px 0 0 0;
    z-index: 25;
    border: none;
    background: rgb(6 12 24 / 50%);
    animation: fade-in var(--dur-2) var(--ease-out);
  }
  .desktop-only {
    display: none;
  }
  main {
    min-width: 0;
    outline: none;
  }
  .nav {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .nav a {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    min-height: 38px;
    padding: 0 var(--space-3);
    border-radius: var(--radius);
    color: var(--side-muted);
    font-size: var(--text-sm);
    font-weight: 500;
    transition:
      background var(--dur-1) var(--ease-out),
      color var(--dur-1) var(--ease-out);
  }
  .nav a:hover {
    background: color-mix(in srgb, var(--side-surface) 70%, transparent);
    color: var(--side-text);
    text-decoration: none;
  }
  .nav a.active {
    background: var(--side-surface);
    color: var(--side-text);
  }
  .nav a :global(svg) {
    opacity: 0.8;
  }
  .instances a {
    min-height: 38px;
    padding-top: 3px;
    padding-bottom: 3px;
  }
  .instance-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.25;
  }
  .instance-text .sub {
    font-size: 0.6875rem;
    color: var(--side-muted);
    font-weight: 400;
  }
  .instances .meter {
    width: 10px;
    height: 10px;
    margin: 0 4px;
  }
  .none {
    padding: var(--space-1) var(--space-3);
    font-size: var(--text-sm);
    color: var(--side-muted);
  }
  .section-title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    min-height: 30px;
    margin-top: var(--space-4);
    padding: 0 0 0 var(--space-3);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--side-muted);
  }
  .status-dot {
    width: 8px;
    height: 8px;
    margin: 0 5px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--side-border);
    box-shadow: inset 0 0 0 1px var(--side-muted);
  }
  .status-dot.connected {
    background: #4fd99a;
    box-shadow: none;
  }
  .status-dot.connecting {
    background: #f2c45a;
    box-shadow: none;
    animation: pulse 1.4s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .account {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding-top: var(--space-3);
    border-top: 1px solid var(--side-border);
  }
  .who {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    min-height: 40px;
    padding: 0 var(--space-2);
    border-radius: var(--radius);
    color: var(--side-text);
    font-size: var(--text-sm);
    font-weight: 500;
  }
  .who:hover,
  .who.active {
    background: var(--side-surface);
    text-decoration: none;
  }
  .avatar {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--side-surface);
    border: 1px solid var(--side-border);
    font-size: var(--text-xs);
    font-weight: 600;
    flex-shrink: 0;
  }
  .build {
    display: flex;
    flex-direction: column;
    padding: var(--space-2) var(--space-2) 0;
    font-size: 0.6875rem;
    line-height: 1.4;
    color: var(--side-muted);
  }
  .build > span {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .build .mono {
    font-size: inherit;
  }
  .topbar-offline {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    margin-left: auto;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius);
    background: #2e2610;
    color: #f2c45a;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .offline {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: #2e2610;
    color: #f2c45a;
  }

  @media (min-width: 900px) {
    .topbar,
    .scrim {
      display: none;
    }
    .desktop-only {
      display: flex;
      padding: var(--space-3) var(--space-3) var(--space-4);
    }
    .sidebar {
      inset: 0 auto 0 0;
      width: var(--sidebar-width);
      transform: none;
      transition: none;
    }
    main {
      margin-left: var(--sidebar-width);
    }
  }
</style>
