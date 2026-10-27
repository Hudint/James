<script lang="ts">
  import '../app.css';
  import { goto, afterNavigate } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
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

  const nav: { href: string; icon: IconName; label: MessageKey; permission?: string }[] = [
    { href: '/', icon: 'dashboard', label: 'nav.dashboard' },
    { href: '/library', icon: 'library', label: 'nav.library', permission: 'library.read' },
    { href: '/playlists', icon: 'list', label: 'nav.playlists', permission: 'playlist.read' },
    { href: '/users', icon: 'users', label: 'nav.users', permission: 'users.manage' },
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
      <a href="/" class="brand"><img src="/favicon.svg" alt="" width="24" height="24" />James</a>
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
      <a href="/" class="brand desktop-only"><img src="/favicon.svg" alt="" width="28" height="28" />James</a>

      <ul class="nav">
        {#each nav.filter((n) => !n.permission || can(n.permission)) as item (item.href)}
          <li>
            <a href={item.href} class:active={active(item.href)} aria-current={active(item.href) ? 'page' : undefined}>
              <Icon name={item.icon} />
              {t(item.label)}
            </a>
          </li>
        {/each}
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
          <li>
            <a
              href="/instances/{instance.id}"
              class:active={active(`/instances/${instance.id}`)}
              aria-current={active(`/instances/${instance.id}`) ? 'page' : undefined}
            >
              <span class="status-dot {dot}" aria-hidden="true"></span>
              <span class="truncate">{instance.name}</span>
              <span class="visually-hidden">({t(dotLabel[dot])})</span>
            </a>
          </li>
        {/each}
      </ul>

      <div class="spacer"></div>

      {#if !live.connected}
        <p class="offline small" role="status"><Icon name="alert" size={16} />{t('common.reconnecting')}</p>
      {/if}

      <ul class="nav">
        <li>
          <a href="/account" class:active={active('/account')}>
            <Icon name="user" />
            <span class="truncate">{session.me.user.username}</span>
          </a>
        </li>
        <li>
          <button onclick={logout}><Icon name="logout" />{t('nav.logout')}</button>
        </li>
      </ul>
    </nav>
    {#if menuOpen}
      <button class="scrim" aria-label={t('common.close')} onclick={() => (menuOpen = false)}></button>
    {/if}

    <!-- While the drawer is open on small screens, the page behind it is not reachable. -->
    <main id="main" tabindex="-1" inert={narrow && menuOpen}>
      {@render children()}
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
    background: var(--accent);
    color: var(--accent-contrast);
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
    gap: var(--space-2);
    font-weight: 700;
    font-size: var(--text-lg);
    color: var(--text);
  }
  .brand:hover {
    text-decoration: none;
  }
  .topbar {
    position: sticky;
    top: 0;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 56px;
    padding: 0 var(--space-2);
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }
  .sidebar {
    position: fixed;
    inset: 56px auto 0 0;
    width: min(var(--sidebar-width), 85vw);
    z-index: 30;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    background: var(--surface);
    border-right: 1px solid var(--border);
    overflow-y: auto;
    transform: translateX(-105%);
    transition: transform 0.18s ease;
  }
  .sidebar.open {
    transform: none;
  }
  .scrim {
    position: fixed;
    inset: 56px 0 0 0;
    z-index: 25;
    border: none;
    background: rgb(0 0 0 / 35%);
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
    gap: 2px;
  }
  .nav a,
  .nav button {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    min-height: 40px;
    padding: 0 var(--space-3);
    border-radius: var(--radius);
    border: none;
    background: none;
    color: var(--text-muted);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: 550;
    text-align: left;
    cursor: pointer;
  }
  .nav a:hover,
  .nav button:hover {
    background: var(--surface-2);
    color: var(--text);
    text-decoration: none;
  }
  .nav a.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .section-title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: var(--space-3);
    padding: 0 var(--space-1) 0 var(--space-3);
    font-size: var(--text-xs);
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
  }
  .status-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--border);
  }
  .status-dot.connected {
    background: var(--success);
  }
  .status-dot.playing {
    background: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .status-dot.connecting {
    background: var(--warning);
  }
  .topbar-offline {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    margin-left: auto;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius);
    background: var(--warning-soft);
    color: var(--warning);
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .status-dot.paused {
    background: var(--accent);
  }
  .offline {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--warning-soft);
    color: var(--warning);
  }

  @media (min-width: 900px) {
    .topbar,
    .scrim {
      display: none;
    }
    .desktop-only {
      display: flex;
      padding: var(--space-2) var(--space-3) var(--space-3);
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
