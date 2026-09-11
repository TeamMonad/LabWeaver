import { describe, expect, it, beforeEach, vi } from 'vitest'
import { defineComponent } from 'vue'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import GcpActionBar from '@/components/common/GcpActionBar.vue'
import GcpFilterBar from '@/components/common/GcpFilterBar.vue'
import GcpProjectSelector from '@/components/layout/GcpProjectSelector.vue'
import { createProject, listProjects } from '@/generated/contracts'
import { useProjects } from '@/composables/useProjects'

const projects = [
  {
    id: 'project-cs101',
    name: 'CS101 Operating Systems',
    description: 'Course project',
    ownerActorId: 'teacher-1',
    courseId: 'cs101-operating-systems',
    state: 'active' as const,
    revision: 1,
    createdAt: '2026-07-11T10:00:00.000Z',
    updatedAt: '2026-07-11T10:00:00.000Z',
  },
  {
    id: 'project-ai201',
    name: 'AI201 Model Engineering',
    description: 'Second project',
    ownerActorId: 'teacher-1',
    courseId: 'ai201-model-engineering',
    state: 'active' as const,
    revision: 1,
    createdAt: '2026-07-11T10:00:00.000Z',
    updatedAt: '2026-07-11T10:00:00.000Z',
  },
]

vi.mock('@/generated/contracts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/generated/contracts')>()
  return { ...actual, createProject: vi.fn(), listProjects: vi.fn() }
})

const ProjectContextHarness = defineComponent({
  components: { GcpProjectSelector },
  setup() {
    const projects = useProjects()
    return { projects }
  },
  template: `
    <div>
      <GcpProjectSelector />
      <ul data-test="existing-project-list">
        <li v-for="project in projects.projects.kind === 'success' ? projects.projects.data : []" :key="project.id">{{ project.id }}</li>
      </ul>
      <button data-test="reload-projects" type="button" @click="projects.load">reload</button>
      <button data-test="create-project" type="button" @click="projects.create('New project')">create</button>
    </div>
  `,
})

describe('GCP Console Components', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(listProjects).mockResolvedValue({ data: projects, error: undefined as never })
    window.localStorage.clear()
  })

  describe('GcpStatusPill', () => {
    it('renders correct label and status class for environment ready state', () => {
      const wrapper = mount(GcpStatusPill, {
        props: { state: 'ready', domain: 'environment' },
      })
      expect(wrapper.text()).toBe('运行中')
      expect(wrapper.find('.gcp-status-pill').classes()).toContain('gcp-status-pill--ready')
    })

    it('renders pulsing indicator for in-progress provisioning state', () => {
      const wrapper = mount(GcpStatusPill, {
        props: { state: 'provisioning', domain: 'environment' },
      })
      expect(wrapper.text()).toBe('置备中')
      expect(wrapper.find('.status-indicator').classes()).toContain('status-indicator--pulse')
    })

    it('renders error class for failed states', () => {
      const wrapper = mount(GcpStatusPill, {
        props: { state: 'failed', domain: 'environment' },
      })
      expect(wrapper.text()).toBe('失败')
      expect(wrapper.find('.gcp-status-pill').classes()).toContain('gcp-status-pill--failed')
    })

    it('supports agent domain state mappings', () => {
      const wrapper = mount(GcpStatusPill, {
        props: { state: 'partially_succeeded', domain: 'agent' },
      })
      expect(wrapper.text()).toBe('部分成功')
    })

    it('supports resource domain state mappings', () => {
      const wrapper = mount(GcpStatusPill, {
        props: { state: 'active', domain: 'resource' },
      })
      expect(wrapper.text()).toBe('使用中')
    })
  })

  describe('GcpActionBar', () => {
    it('emits refresh event when refresh button is clicked', async () => {
      const wrapper = mount(GcpActionBar)
      const refreshBtn = wrapper.find('.action-button')
      expect(refreshBtn.exists()).toBe(true)
      await refreshBtn.trigger('click')
      expect(wrapper.emitted('refresh')).toBeTruthy()
    })

    it('renders action buttons in default slot', () => {
      const wrapper = mount(GcpActionBar, {
        slots: {
          default: '<button class="test-btn">新建</button>',
        },
      })
      expect(wrapper.find('.test-btn').exists()).toBe(true)
      expect(wrapper.find('.test-btn').text()).toBe('新建')
    })

    it('toggles auto-refresh when checkbox changes', async () => {
      const wrapper = mount(GcpActionBar, {
        props: { showAutoRefresh: true, autoRefresh: false },
      })
      const checkbox = wrapper.find('input[type="checkbox"]')
      expect(checkbox.exists()).toBe(true)
      await checkbox.setValue(true)
      expect(wrapper.emitted('update:autoRefresh')?.[0]).toEqual([true])
    })
  })

  describe('GcpFilterBar', () => {
    it('emits filterChange when entering search text', async () => {
      const wrapper = mount(GcpFilterBar, {
        props: {
          modelValue: '',
          placeholder: '过滤表格',
        },
      })
      const input = wrapper.find('input')
      await input.setValue('running')
      expect(wrapper.emitted('update:modelValue')?.[0]).toEqual(['running'])
      expect(wrapper.emitted('filterChange')).toBeTruthy()
    })

    it('applies quick preset when preset button is clicked', async () => {
      const wrapper = mount(GcpFilterBar, {
        props: {
          modelValue: '',
          presets: [{ label: '运行中', key: 'state', value: 'ready' }],
        },
      })
      const presetBtn = wrapper.find('.preset-btn')
      expect(presetBtn.exists()).toBe(true)
      await presetBtn.trigger('click')
      expect(wrapper.emitted('filterChange')).toBeTruthy()
    })
  })

  describe('GcpProjectSelector', () => {
    it('displays the selected project and its course association', async () => {
      const wrapper = mount(GcpProjectSelector)
      await vi.waitFor(() => expect(wrapper.text()).toContain('CS101 Operating Systems'))
      expect(wrapper.text()).toContain('project-cs101')
    })

    it('opens dropdown when trigger is clicked', async () => {
      const wrapper = mount(GcpProjectSelector)
      expect(wrapper.find('.selector-menu').exists()).toBe(false)
      await wrapper.find('.selector-trigger').trigger('click')
      expect(wrapper.find('.selector-menu').exists()).toBe(true)
    })

    it('switches project context on selecting an option from the catalog', async () => {
      const wrapper = mount(GcpProjectSelector)
      await wrapper.find('.selector-trigger').trigger('click')
      await vi.waitFor(() => expect(wrapper.findAll('.project-item')).toHaveLength(2))
      const items = wrapper.findAll('.project-item')
      expect(items.length).toBeGreaterThan(0)
      await items[1].trigger('click')
      expect(wrapper.find('.trigger-primary').text()).toBe('AI201 Model Engineering')
    })

    it('shares the authoritative catalog with existing project consumers after creation', async () => {
      let catalog = [...projects]
      const createdProject = {
        ...projects[0],
        id: 'project-created',
        name: 'New project',
      }
      vi.mocked(listProjects).mockImplementation(async () => ({ data: catalog, error: undefined as never }))
      vi.mocked(createProject).mockImplementation(async () => {
        catalog = [...catalog, createdProject]
        return { data: createdProject, error: undefined as never }
      })

      const wrapper = mount(ProjectContextHarness)
      try {
        await wrapper.get('[data-test="reload-projects"]').trigger('click')
        await vi.waitFor(() => expect(wrapper.get('[data-test="existing-project-list"]').text()).toContain('project-cs101'))

        await wrapper.find('.selector-trigger').trigger('click')
        await vi.waitFor(() => expect(wrapper.findAll('.selector-menu .project-item')).toHaveLength(2))

        await wrapper.get('[data-test="create-project"]').trigger('click')
        await vi.waitFor(() => expect(wrapper.get('[data-test="existing-project-list"]').text()).toContain('project-created'))

        if (!wrapper.find('.selector-menu').exists()) await wrapper.find('.selector-trigger').trigger('click')
        await vi.waitFor(() => expect(wrapper.findAll('.selector-menu .project-item')).toHaveLength(3))
        expect(wrapper.find('.trigger-primary').text()).toBe('New project')
      } finally {
        wrapper.unmount()
      }
    })
  })
})
