<template>
  <div>{{ message }}</div>
</template>

<script lang="ts">
import { ref } from 'vue'

export default {
  name: 'TestComponent',
  setup() {
    const message = ref('Hello')
    return { message }
  }
}
</script>
