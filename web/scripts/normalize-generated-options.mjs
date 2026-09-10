import { readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'

const outputRoot = resolve(process.argv[2] ?? 'src/generated/contracts')
const path = resolve(outputRoot, 'client/types.gen.ts')
const source = await readFile(path, 'utf8')

const pattern = /export type Options<\n  TData extends TDataShape = TDataShape,\n  ThrowOnError extends boolean = boolean,\n  TResponse = unknown,\n> = OmitKeys<RequestOptions<TResponse, ThrowOnError>, 'body' \| 'path' \| 'query' \| 'url'> &\n  \(\[TData\] extends \[never\] \? unknown : Omit<TData, 'url'>\);/

const replacement = `type BrowserRequestHeaders<T> = T extends Record<string, unknown>
  ? Omit<T, 'Origin' | 'X-CSRF-Token'> &
      Partial<Pick<T, Extract<keyof T, 'Origin' | 'X-CSRF-Token'>>>
  : T;

type BrowserRequestData<TData extends TDataShape> = [TData] extends [never]
  ? unknown
  : Omit<TData, 'url' | 'headers'> &
      ('headers' extends keyof TData
        ? { headers: BrowserRequestHeaders<NonNullable<TData['headers']>> }
        : unknown);

export type Options<
  TData extends TDataShape = TDataShape,
  ThrowOnError extends boolean = boolean,
  TResponse = unknown,
> = OmitKeys<RequestOptions<TResponse, ThrowOnError>, 'body' | 'path' | 'query' | 'url' | 'headers'> &
  Pick<RequestOptions<TResponse, ThrowOnError>, 'headers'> &
  BrowserRequestData<TData>;`

if (!pattern.test(source)) {
  if (source.includes('type BrowserRequestData<TData extends TDataShape>')) process.exit(0)
  throw new Error(`Generated client Options declaration was not found in ${path}`)
}

await writeFile(path, source.replace(pattern, replacement))
