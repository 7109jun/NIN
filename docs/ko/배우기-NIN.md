# Learn NIN — NIN 배우기

NIN Shell은 Windows x64용 셸이자 스크립팅 언어입니다. 리눅스는 지원하지만 버그가 있을 가능성이 높습니다. (검증하지 못하였습니다. 리눅스를 원한다면 fork한후 새로운 버전을 만들으시면 됩니다. 라이선스 파일을 잘 따라야합니다.)

NIN 프로그램은 `.nin` 확장자를 사용하는 JSON 문서입니다. 일반적인 JSON과 달리 `//` 주석과 마지막 항목 뒤의 쉼표(trailing comma)를 허용합니다.

이 튜토리얼에서는 간단한 Hello World부터 실제 워크플로까지 NIN을 단계별로 살펴봅니다.

대상 버전: NIN Shell 0.1.0

일부 프로세스 실행 예제의 `cmd`는 Windows 환경이 필요합니다.

## 1. 명령 실행하기

NIN을 실행하는 방법은 다음과 같습니다.

```
nin                       # 대화형 셸(REPL) 실행
nin run hello.nin         # 스크립트 실행 (작업 디렉터리는 스크립트가 있는 폴더)
nin hello.nin             # 위 명령과 동일하며, .nin 파일을 더블 클릭해도 실행됨
nin check hello.nin       # 실행하지 않고 유효성 검사 및 분석 수행
nin eval "1 + 2 * 3"      # 하나의 표현식 평가
```

가장 간단한 NIN 프로그램은 다음과 같습니다.

hello.nin

```
{
  "nin": "1",
  "main": [
    {"print": "Hello, NIN!"}
  ]
}
```

`"nin": "1"`은 필수 키입니다.

`main`은 실행할 명령문(statement)의 배열입니다. 각 명령문은 하나의 주요 키(`print`, `let`, `if` 등)와 추가적인 수정자 키를 가진 객체입니다.

`.nin` 파일을 더블 클릭하여 실행하려면 다음 명령을 한 번 실행해야 합니다.

```
nin assoc install
```

`.nin` 파일은 해당 파일이 위치한 폴더를 작업 디렉터리로 사용하여 실행됩니다. 실행이 끝나면 키 입력을 기다리므로 콘솔 창이 바로 닫히지 않습니다.

## 2. 값과 `${}`

NIN에서 사용할 수 있는 값의 종류는 다음과 같습니다.

- `null`
- 불리언(Boolean)
- 64비트 정수(Integer)
- 부동소수점(Float)
- 문자열(String)
- 리스트(List)
- 맵(Map)
- 순서가 유지되는 문자열 키 객체
- 함수(Function)
- 작업(Task)

`value`, `print`, `return`, `args` 등의 값 위치에서는 문자열 안에 `${expression}`을 사용할 수 있습니다.

```
{
  "let": "n",
  "value": 4
},
{
  "print": "n squared is ${n * n}"
},
{
  "let": "xs",
  "value": "${[3,1,2].sort()}"
}
```

문자열 전체가 `${...}` 하나로만 구성되어 있다면 결과는 문자열이 아닌 실제 평가된 타입으로 반환됩니다.

그 외에는 일반 문자열로 처리됩니다.

문자열에 `${`를 문자 그대로 포함하려면 `$${`를 사용합니다.

표현식 위치에서는 문자열 자체가 표현식으로 해석됩니다. 다음과 같은 위치가 해당합니다.

- `if`
- `while`
- `for ... in`
- `match`
- `call`
- `set`의 대상

예를 들어 다음과 같이 작성할 수 있습니다.

```
{
  "if": "n > 3 && ready"
}
```

지원하는 연산자는 다음과 같습니다.

\| 종류 | 연산자 |\
\|---|---|\
\| 산술 | `+`, `-`, `*`, `/`, `%`, `**` |\
\| 비교 | 비교 연산자 |\
\| 논리 | `&&`, `||`, `!` |\
\| 널 병합 | `??` |\
\| 삼항 | `?:` |\
\| 포함 여부 | `in` |\
\| 타입 확인 | `is` |\
\| 타입 변환 | `as` |\
\| 안전한 접근 | `?.` |\
\| 인덱싱 및 슬라이싱 | 인덱스 및 범위 접근 |

나눗셈은 피연산자에 따라 정수 또는 실수 결과를 반환합니다.

- `6 / 3` → 정수 `2`
- `7 / 2` → 실수 `3.5`

조건식은 불리언 또는 `null`이어야 합니다. 그 외의 타입을 사용하면 `TypeError`가 발생합니다.

## 3. 변수와 제어 흐름

NIN은 변수 선언, 상수 선언, 반복문, 조건문, 맵 순회 및 패턴 매칭을 지원합니다.

변수 선언에는 `let`, 상수 선언에는 `const`를 사용합니다.

반복문에서는 `break`와 `continue`를 사용할 수 있습니다.

`set`은 기존 값을 변경합니다. 다음과 같은 대상을 지정할 수 있습니다.

- `x`
- `a.b`
- `xs[0]`
- `this.n`

선택적 연산자도 지원합니다.

리스트와 맵에는 다양한 내장 메서드가 제공됩니다.

- `map`
- `filter`
- `reduce`
- `sort`
- `join`
- `keys`
- `values`

람다 함수는 다음과 같이 작성할 수 있습니다.

```
fn(n) => n * n
```

## 4. 함수

NIN은 함수를 지원하며, 함수는 JSON 명령문을 통해 정의할 수 있습니다.

함수 매개변수는 다음 기능을 지원합니다.

- 일반 매개변수
- 타입이 지정된 매개변수
- 기본값이 지정된 매개변수
- `...rest`를 사용한 가변 인자

타입 시스템은 점진적 타입 지정(Gradual Typing)을 지원합니다. 타입을 생략하면 기본적으로 `any`가 사용됩니다.

함수를 비동기 함수로 선언하려면 `async: true`를 지정합니다.

비동기 함수는 작업(Task)을 반환하며, `await`를 사용해 결과를 기다릴 수 있습니다.

## 5. 오류 처리

NIN은 `try`, `throw`, `catch`, `as`, `type`, `finally`를 사용한 예외 처리를 지원합니다.

내장 오류 계층은 다음과 같습니다.

```
Error
├── TypeError
├── ValueError
├── KeyError
├── IndexError
├── ArithmeticError
├── AssertionError
├── IOError
├── ProcessError
├── TimeoutError
├── CancelledError
├── StateError
├── AccessError
├── ImportError
└── NameError
```

처리되지 않은 오류가 발생하면 NIN은 진단 정보를 출력하고 NIN 호출 스택을 표시합니다.

프로그램은 종료 코드 `1`로 종료됩니다.

`assert`는 조건이 거짓일 때 `AssertionError`를 발생시킵니다.

## 6. 클래스와 객체

NIN은 객체 지향 프로그래밍(OOP)을 지원합니다.

다음 기능을 사용할 수 있습니다.

- 인터페이스(Interface)
- 클래스(Class)
- 추상 클래스(Abstract Class)
- 상속(Inheritance)
- 다형성(Polymorphism)
- 메서드 오버로딩(Method Overloading)
- 제네릭(Generics)
- 리플렉션(Reflection)
- 열거형(Enum)

인터페이스는 `Speaker`와 같이 정의할 수 있습니다.

클래스는 `Animal`처럼 정의할 수 있으며, 추상 클래스와 인터페이스 구현을 지원합니다.

예를 들어 `Animal` 클래스를 상속하는 `Dog` 클래스를 만들 수 있습니다.

객체는 `new`를 사용하여 생성합니다.

```
new Dog("Rex")
```

캡슐화(Encapsulation)

접근 제한자는 다음과 같습니다.

- `public`: 공개
- `protected`: 보호
- `private`: 비공개

기본 접근 제한자는 `public`입니다.

접근 권한을 위반하면 `AccessError`가 발생합니다. 정적 분석기가 위반을 확인할 수 있는 경우에는 정적 오류로도 보고됩니다.

상속과 다형성(Inheritance and Polymorphism)

- 하나의 클래스를 `extends`로 상속할 수 있습니다.
- 여러 인터페이스를 `implements`로 구현할 수 있습니다.
- 상속받은 구체적인 메서드를 재정의하려면 `override`가 필요합니다.
- 부모 클래스의 메서드는 `super.m()`으로 호출할 수 있습니다.

추상 클래스와 인터페이스(Abstract Classes and Interfaces)

추상 클래스와 인터페이스는 직접 인스턴스화할 수 없습니다.

구체적인 하위 클래스는 모든 추상 메서드를 구현해야 합니다.

오버로딩(Overloading)

메서드는 배열 형태의 여러 오버로드를 가질 수 있습니다.

호출할 메서드는 매개변수 개수를 먼저 확인하고, 그다음 매개변수 타입을 기준으로 선택됩니다.

제네릭(Generics)

제네릭은 다음과 같이 선언할 수 있습니다.

```
{
  "generics": ["T"]
}
```

제네릭 타입은 다음과 같이 사용할 수 있습니다.

```
new Box<int>(7)
```

타입 인자는 런타임에도 유지됩니다.

리플렉션(Reflection)

리플렉션은 다음 기능을 제공합니다.

- `reflect.class_name`
- `reflect.fields`
- `reflect.methods`
- `reflect.invoke`

이를 통해 클래스 이름, 필드, 메서드 정보를 확인하고 메서드를 호출할 수 있습니다.

열거형(Enums)

`Color`와 같은 열거형을 정의할 수 있습니다.

## 7. 모듈과 표준 라이브러리

NIN은 `import`를 통해 모듈을 불러올 수 있습니다.

예를 들어 다음과 같이 사용할 수 있습니다.

```
import std.fs
import std.json as j
import ./util.nin
import mypkg
```

모듈 파일은 함수, 클래스, 열거형 및 전역 변수를 내보냅니다.

불러온 모듈의 요소는 지정된 별칭을 통해 접근할 수 있습니다.

모듈 파일에 정의된 자체 `main`은 모듈을 가져올 때 실행되지 않습니다.

NIN 표준 라이브러리는 다음 모듈을 제공합니다.

| 모듈        | 기능      |
| --------- | ------- |
| `fs`      | 파일 시스템  |
| `path`    | 경로 처리   |
| `process` | 프로세스 관리 |
| `net`     | 네트워크    |
| `sys`     | 시스템 기능  |
| `json`    | JSON    |
| `yaml`    | YAML    |
| `toml`    | TOML    |
| `object`  | 객체      |
| `task`    | 작업      |
| `time`    | 시간      |
| `math`    | 수학      |
| `str`     | 문자열     |
| `list`    | 리스트     |
| `map`     | 맵       |
| `regex`   | 정규 표현식  |
| `log`     | 로깅      |
| `io`      | 입출력     |
| `env`     | 환경 변수   |
| `hash`    | 해시      |
| `random`  | 난수      |

## 8. 다른 프로그램 실행하기

NIN은 외부 프로그램을 실행할 수 있습니다.

`run`을 사용하여 실행할 프로그램과 인자를 지정할 수 있습니다.

다음 기능을 지원합니다.

- 프로그램 실행
- 인자 전달
- 작업 디렉터리 지정(`into`)
- 시간 제한(`timeout`)
- 실행 결과 캡처

예를 들어 Git을 실행하고 결과를 받아올 수 있습니다.

```
{
  "run": "git",
  "args": ["status"],
  "into": "r"
},
{
  "print": "Exit code: ${r.code}"
},
{
  "print": "${r.stdout}"
}
```

Windows의 `cmd`를 실행하는 예제입니다.

```
{
  "run": "cmd",
  "args": ["/c", "dir"],
  "stdout": "file"
}
```

프로세스 실행에 `check: true`를 지정하면 프로세스가 실패했을 때 `ProcessError`가 발생합니다.

```
{
  "run": "build.exe",
  "check": true
}
```

인자는 실제 argv 리스트로 전달됩니다. 인자 문자열을 다시 분리하거나 재해석하지 않습니다.

`into`를 사용하면 다음 정보를 가진 객체가 반환됩니다.

| 필드            | 설명         |
| ------------- | ---------- |
| `code`        | 종료 코드      |
| `ok`          | 성공 여부      |
| `stdout`      | 표준 출력      |
| `stderr`      | 표준 오류      |
| `duration_ms` | 실행 시간(밀리초) |
| `timed_out`   | 시간 초과 여부   |
| `pid`         | 프로세스 ID    |

`into` 또는 출력 리디렉션을 지정하지 않으면 출력은 콘솔에 표시됩니다.

## 9. 동시성

NIN은 여러 작업을 동시에 실행할 수 있습니다.

다음 기능을 지원합니다.

- `parallel`
- `spawn`
- `await`

`parallel`은 여러 분기를 동시에 실행하고 모든 분기가 완료될 때까지 기다립니다.

`spawn`은 작업을 시작하고 작업 핸들을 반환합니다.

`await`는 해당 작업이 완료될 때까지 기다립니다. 작업에서 오류가 발생했다면 그 오류를 다시 발생시킵니다.

`Ctrl+C`를 누르면 실행 중인 작업이 취소되며 `CancelledError`가 발생합니다.

## 10. 워크플로

NIN은 작업(Job)과 단계(Step)로 구성된 워크플로를 지원합니다.

각 작업은 여러 단계의 목록을 가집니다.

선행 작업이 완료되지 않은 작업은 시작되지 않습니다. 충족되지 않은 의존성이 없는 작업은 자동으로 시작되며, 여러 작업이 동시에 실행될 수 있습니다.

다음은 워크플로의 JSON 예제입니다.

```
{
  "workflow": "ci",
  "inputs": {
    "profile": {
      "type": "string",
      "default": "debug"
    }
  },
  "jobs": {
    "lint": {
      "steps": []
    },
    "test": {
      "matrix": {
        "os": ["a", "b"]
      },
      "retries": 1,
      "timeout": "5m",
      "steps": []
    },
    "pack": {
      "needs": ["lint", "test"],
      "outputs": {
        "artifact": "..."
      },
      "finally": []
    }
  }
}
```

워크플로 관련 명령은 다음과 같습니다.

```
nin workflow list
nin workflow graph
nin workflow run --input profile=release --max-parallel 4 --report report.json
```

특정 작업만 실행하려면 `--job`을 사용합니다.

```
nin workflow run --job pack
```

실제 실행 없이 워크플로를 확인하려면 `--dry-run`을 사용합니다.

```
nin workflow run --dry-run
```

워크플로 단계는 다음 세 가지 방식으로 실행할 수 있습니다.

| 종류       | 설명             |
| -------- | -------------- |
| `run`    | 외부 프로세스 실행     |
| `script` | NIN 명령문 실행     |
| `uses`   | 인자를 전달하여 함수 호출 |

각 단계는 다음과 같은 조건식을 지원합니다.

- `success()`
- `failure()`
- `always()`
- `cancelled()`

또한 다음 설정을 사용할 수 있습니다.

- `timeout`
- `retries`
- `retry_delay`
- `continue_on_error`
- `on_error`

작업의 리소스 사용량은 제한할 수 있습니다.

- 메모리
- 프로세스 수
- CPU 실행 시간

Windows에서는 Job Object를 사용하여 이러한 제한을 적용합니다.

워크플로는 전체 실행이 성공한 경우에만 종료 코드 `0`을 반환합니다.

## 11. 패키지, 플러그인 및 예약 작업

NIN은 패키지 관리 기능을 제공합니다.

패키지 관련 명령은 다음과 같습니다.

```
nin pkg pack
nin pkg install
nin pkg list
nin pkg info
nin pkg update
nin pkg remove
nin pkg search
```

플러그인 정보를 확인할 수도 있습니다.

```
nin plugin info hello.dll
```

예약 작업은 다음과 같이 등록할 수 있습니다.

```
nin schedule add nightly "daily 02:30" backup.nin
```

예약 작업 관련 명령은 다음과 같습니다.

```
nin schedule list
nin schedule run
nin schedule daemon
nin schedule register
```

Windows에서는 Windows 작업 스케줄러(Task Scheduler)를 사용하여 예약 작업을 등록할 수 있습니다.

플러그인 및 모듈을 작성하는 방법은 Module Development Method 문서를 참고하십시오.

모든 명령과 키에 대한 자세한 설명은 Reference 문서를 참고하십시오.

## 12. 실행 파일로 컴파일하기

NIN 프로그램은 독립 실행 파일로 빌드할 수 있습니다.

```
nin build app.nin -o app.exe
```

생성되는 독립 실행 파일에는 다음 요소가 포함됩니다.

- NIN 런타임
- 검증된 프로그램
- 모든 가져온 파일
- 모든 패키지

따라서 별도의 NIN 런타임 설치 없이 실행할 수 있는 실행 파일을 만들 수 있습니다.

## 13. REPL

NIN은 대화형 셸(REPL)을 제공합니다.

REPL에서 표현식을 입력하면 평가 결과가 반환됩니다.

`_`는 가장 최근에 평가된 결과를 나타냅니다.

한 줄짜리 JSON 명령문도 실행할 수 있습니다.

여러 줄에 걸친 입력도 지원합니다. 괄호가 닫히지 않은 상태라면 다음 줄의 입력을 계속 기다립니다.

REPL에서는 다음 명령을 사용할 수 있습니다.

| 명령           | 설명         |
| ------------ | ---------- |
| `:help`      | 도움말        |
| `:vars`      | 변수 목록      |
| `:funcs`     | 함수 목록      |
| `:classes`   | 클래스 목록     |
| `:load FILE` | 파일 불러오기    |
| `:reset`     | 초기화        |
| `:cd DIR`    | 작업 디렉터리 변경 |
| `:history`   | 명령 기록      |
| `:quit`      | 종료         |

`!dir`처럼 `!`로 시작하는 명령은 셸 명령으로 실행할 수 있습니다.
