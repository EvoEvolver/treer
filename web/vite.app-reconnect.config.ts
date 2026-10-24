import { mergeConfig } from 'vite'
import base from './vite.config'
// Exercise production runtime Proxy-origin configuration in the component fixture.
export default mergeConfig(base, { define: { 'import.meta.env.DEV': 'false' } })
