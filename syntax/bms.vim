" 最小 BMS 语法高亮；完整语义由 bms-lsp 的 semantic tokens 提供。
if exists("b:current_syntax")
  finish
endif

syntax match bmsComment /\/\/.*$/
syntax match bmsComment /^\s*;.*$/
syntax region bmsComment start=/\/\*/ end=/\*\//
syntax match bmsControl /^\s*#\(RANDOM\|SETRANDOM\|ENDRANDOM\|IF\|ELSEIF\|ELSE\|ENDIF\|SWITCH\|SETSWITCH\|CASE\|SKIP\|DEF\|ENDSW\)\>/
syntax match bmsMessage /^\s*#\d\{3}[0-9A-Za-z]\{2\}:.*$/
syntax match bmsHeader /^\s*[#%][A-Za-z0-9/@]*/

highlight default link bmsComment Comment
highlight default link bmsControl Keyword
highlight default link bmsMessage String
highlight default link bmsHeader Function

let b:current_syntax = "bms"
