; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_toFahrenheit(i32 %arg0) {
entry:
  %t0 = add i32 0, 9
  %t1 = mul i32 %arg0, %t0
  %t2 = add i32 0, 5
  %t3 = sdiv i32 %t1, %t2
  %t4 = add i32 0, 32
  %t5 = add i32 %t3, %t4
  ret i32 %t5
}

define i32 @InputKt_toCelsius(i32 %arg0) {
entry:
  %t0 = add i32 0, 32
  %t1 = sub i32 %arg0, %t0
  %t2 = add i32 0, 5
  %t3 = mul i32 %t1, %t2
  %t4 = add i32 0, 9
  %t5 = sdiv i32 %t3, %t4
  ret i32 %t5
}

define i32 @main() {
entry:
  %t0 = add i32 0, 0
  %t1 = call i32 @InputKt_toFahrenheit(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, 100
  %t4 = call i32 @InputKt_toFahrenheit(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 32
  %t7 = call i32 @InputKt_toCelsius(i32 %t6)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t7)
  %t9 = add i32 0, 212
  %t10 = call i32 @InputKt_toCelsius(i32 %t9)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t10)
  ret i32 0
}

