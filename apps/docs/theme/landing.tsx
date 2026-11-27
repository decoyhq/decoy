import { useState } from 'react'
import { useI18n, useLang, useSite, withBase } from '@rspress/core/runtime'
import type { LandingMessages } from '../rspress.config'

export function Landing() {
  const [missing, setMissing] = useState(false)
  const t = useI18n<LandingMessages>()
  const lang = useLang()
  const { site } = useSite()
  const docs = lang === site.lang ? '/docs/' : `/${lang}/docs/`
  return (
    <main className="pond-page">
      <div className="pond-layout">
        <div className="pond-intro">
          <h1>
            {t('landing.sameRequest')}
            <br />
            {t('landing.differentAnswer')}
          </h1>
          <p>{t('landing.description')}</p>
          <div className="pond-links">
            <a href={withBase(`${docs}quick-start/`)}>{t('landing.quickStart')}</a>
            <a href={withBase(docs)}>{t('landing.docs')}</a>
          </div>
        </div>
        <section className="pond-demo" aria-label={t('landing.exampleResponse')}>
          <div className="pond-water" aria-hidden="true">
            <svg viewBox="0 0 460 145" preserveAspectRatio="xMidYMax meet">
              <g fill="none" stroke="currentColor">
                <ellipse cx="231" cy="109" rx="104" ry="14" />
                <ellipse cx="231" cy="109" rx="148" ry="22" strokeDasharray="318 54 137 180" />
                <ellipse cx="231" cy="109" rx="205" ry="32" strokeDasharray="123 105 227 300" />
              </g>
              <image href={withBase('/brand/decoy.svg')} x="153" y="-15" width="155" height="155" />
            </svg>
          </div>
          <div className="pond-inspector">
            <div className="pond-request">
              <strong>GET</strong> /api/users/1
            </div>
            <div className="pond-response" aria-live="polite" aria-atomic="true">
              <b className={missing ? 'pond-miss' : 'pond-ok'}>
                {missing ? '404 Not Found' : '200 OK'}
              </b>
              <pre>
                {missing
                  ? '{\n  "error": "user not found"\n}'
                  : '{\n  "id": "1",\n  "name": "Ada Lovelace"\n}'}
              </pre>
            </div>
            <fieldset>
              <legend>{t('landing.switchCollection')}</legend>
              {['happy-path', 'no-users'].map((name, i) => (
                <label key={name}>
                  <input
                    type="radio"
                    name="collection"
                    checked={missing === Boolean(i)}
                    onChange={() => {
                      setMissing(Boolean(i))
                    }}
                  />
                  {name}
                </label>
              ))}
            </fieldset>
            <code className="pond-command">
              › /collection {missing ? 'no-users' : 'happy-path'}
            </code>
          </div>
        </section>
      </div>
    </main>
  )
}
