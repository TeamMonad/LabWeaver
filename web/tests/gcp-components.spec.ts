import { describe, expect, it, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import GcpStatusPill from '@/components/common/GcpStatusPill.vue'
import GcpActionBar from '@/components/common/GcpActionBar.vue'
import GcpFilterBar from '@/components/common/GcpFilterBar.vue'
import GcpProjectSelector from '@/components/layout/GcpProjectSelector.vue'
import { useCourseStore } from '@/stores/course'

describe('GCP Console Components', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
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
    it('displays active course ID from store', () => {
      const store = useCourseStore()
      store.setContext('cs101-operating-systems')
      const wrapper = mount(GcpProjectSelector)
      expect(wrapper.text()).toContain('cs101-operating-systems')
    })

    it('opens dropdown when trigger is clicked', async () => {
      const wrapper = mount(GcpProjectSelector)
      expect(wrapper.find('.selector-menu').exists()).toBe(false)
      await wrapper.find('.selector-trigger').trigger('click')
      expect(wrapper.find('.selector-menu').exists()).toBe(true)
    })

    it('switches course context on selecting an option from catalog', async () => {
      const store = useCourseStore()
      const wrapper = mount(GcpProjectSelector)
      await wrapper.find('.selector-trigger').trigger('click')
      const items = wrapper.findAll('.course-item')
      expect(items.length).toBeGreaterThan(0)
      await items[0].trigger('click')
      expect(store.currentContext?.courseId).toBeDefined()
    })
  })
})
